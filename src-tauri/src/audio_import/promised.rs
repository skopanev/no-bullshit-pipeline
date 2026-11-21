//! Bridge modern file URLs and AppKit file promises into audio import. Wry's
//! macOS collector only reads NSFilenamesPboardType, which is absent on some
//! app-to-app drags (including promised exports).
use super::{AUDIO_EXTENSIONS, StagingDir};
use block2::RcBlock;
use objc2::runtime::{AnyClass, AnyObject, Bool, ProtocolObject, Sel};
use objc2::{ClassType, Message, msg_send, sel};
use objc2_app_kit::{NSDraggingInfo, NSFilePromiseReceiver, NSPasteboard, NSView};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSOperationQueue, NSString, NSURL};
use serde::Serialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use tauri::{Emitter, Manager};
use uuid::Uuid;

const MAX_DROP_FILES: usize = 64;

type PerformDrop = unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> Bool;

struct DropHook {
    class: &'static AnyClass,
    original: PerformDrop,
}

static DROP_HOOK: OnceLock<DropHook> = OnceLock::new();

thread_local! {
    // Only registered main views get the native override. Other webviews keep
    // Wry's original behavior. Access and teardown stay on the main thread.
    static DROP_APPS: RefCell<HashMap<usize, tauri::AppHandle>> = RefCell::new(HashMap::new());
    // AppKit objects stay on the main thread and are released explicitly after
    // the final callback, rather than capturing a receiver in its own block.
    static ACTIVE_PROMISES: RefCell<HashMap<String, (objc2::rc::Retained<NSFilePromiseReceiver>, objc2::rc::Retained<NSOperationQueue>)>> = RefCell::new(HashMap::new());
}

#[derive(Clone, Serialize)]
struct NativeImportEvent {
    id: String,
    phase: &'static str,
    title: String,
    recording_id: Option<String>,
    error: Option<String>,
}

fn promise_metadata(receiver: &NSFilePromiseReceiver) -> (Vec<String>, usize) {
    // Voice Memos may return nil before materializing the promise, despite
    // AppKit's nonnull annotation. The generated fileNames() getter panics on
    // that real-world response, so explicitly decode both getters as nullable.
    let names: Option<objc2::rc::Retained<NSArray<NSString>>> =
        unsafe { objc2::msg_send![receiver, fileNames] };
    let types: Option<objc2::rc::Retained<NSArray<NSString>>> =
        unsafe { objc2::msg_send![receiver, fileTypes] };
    let names: Vec<String> = names
        .map(|n| n.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();
    let count = names.len().max(types.map(|t| t.len()).unwrap_or(0)).max(1);
    (names, count)
}

fn event(id: &str, phase: &'static str, title: &str) -> NativeImportEvent {
    NativeImportEvent {
        id: id.into(),
        phase,
        title: title.into(),
        recording_id: None,
        error: None,
    }
}

fn emit(app: &tauri::AppHandle, payload: NativeImportEvent) {
    if let Err(error) = app.emit("audio-import-native", payload) {
        log::warn!("native_drop: cannot notify main window: {error}");
    }
}

fn error(app: &tauri::AppHandle, id: &str, title: &str, message: String) {
    let mut payload = event(id, "error", title);
    payload.error = Some(message);
    emit(app, payload);
}

struct ReceivingFile {
    app: tauri::AppHandle,
    id: String,
    title: String,
    remaining: Arc<AtomicUsize>,
}

impl Drop for ReceivingFile {
    fn drop(&mut self) {
        if self
            .remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .ok()
            == Some(1)
        {
            // Remove the parent only AFTER the imported child/error event has
            // been queued, so the list never flashes an empty state mid-drop.
            emit(&self.app, event(&self.id, "received", &self.title));
            let id = self.id.clone();
            let _ = self.app.run_on_main_thread(move || {
                ACTIVE_PROMISES.with(|active| {
                    active.borrow_mut().remove(&id);
                });
            });
        }
    }
}

pub(crate) fn install(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    let app = window.app_handle().clone();
    window.with_webview(move |webview| {
        // SAFETY: Tauri's main-thread callback supplies a live WKWebView,
        // which inherits NSView. No native pointer leaves this callback.
        let view = unsafe { &*webview.inner().cast::<NSView>() };
        let mut types: Vec<_> = view.registeredDraggedTypes().iter().collect();
        types.extend(NSFilePromiseReceiver::readableDraggedTypes().iter());
        types.push(NSString::from_str("public.file-url"));
        view.registerForDraggedTypes(&NSArray::from_retained_slice(&types));
        let object = unsafe { &*webview.inner().cast::<AnyObject>() };
        // Do not change a live WKWebView's class: its existing drag destination
        // registration and inherited enter/update handlers must stay intact.
        install_drop_hook(object.class());
        DROP_APPS.with(|apps| {
            apps.borrow_mut()
                .insert(object as *const AnyObject as usize, app);
        });
        log::info!(
            "native_drop: installed synchronous receiver on {:?}; {} drag types",
            object.class().name(),
            view.registeredDraggedTypes().len()
        );
    })?;
    Ok(())
}

fn install_drop_hook(class: &'static AnyClass) {
    let hook = DROP_HOOK.get_or_init(|| {
        let method = class
            .instance_method(sel!(performDragOperation:))
            .expect("Wry webview must implement performDragOperation:");
        // SAFETY: Wry and our replacement both implement BOOL(id, SEL, id).
        // Install only on the main thread before any drag begins, preserving
        // the original IMP (not a recursive lookup through the changed class).
        let original = unsafe {
            std::mem::transmute::<objc2::runtime::Imp, PerformDrop>(method.implementation())
        };
        unsafe {
            method.set_implementation(std::mem::transmute::<PerformDrop, objc2::runtime::Imp>(
                perform_drop,
            ));
        }
        DropHook { class, original }
    });
    assert_eq!(
        class, hook.class,
        "native drop hook must stay on the Wry class"
    );
}

unsafe extern "C-unwind" fn perform_drop(this: &AnyObject, cmd: Sel, info: *mut AnyObject) -> Bool {
    // Preserve Wry's normal path imports and overlay events. Its event is
    // queued, but we must request promises BEFORE this native method returns:
    // AppKit throws if receivePromisedFiles is called from a later Tauri event.
    let original = DROP_HOOK.get().expect("drop hook installed").original;
    let accepted = unsafe { original(this, cmd, info) };
    let app = DROP_APPS.with(|apps| {
        apps.borrow()
            .get(&(this as *const AnyObject as usize))
            .cloned()
    });
    if let Some(app) = app {
        // SAFETY: AppKit provides id<NSDraggingInfo> during this synchronous
        // method. The pointer never escapes the callback.
        let Some(info) = (unsafe { info.cast::<ProtocolObject<dyn NSDraggingInfo>>().as_ref() })
        else {
            return accepted;
        };
        if let Err(failure) = native_call(|| {
            let pasteboard = info.draggingPasteboard();
            if !has_legacy_paths(&pasteboard) {
                receive_drop(&app, &pasteboard);
            }
        }) {
            log::error!("native_drop: {failure}");
            error(&app, "native-drop", "Audio import", failure);
        }
    }
    accepted
}

fn native_call<T>(call: impl FnOnce() -> T) -> Result<T, String> {
    // Rust catch_unwind cannot catch Objective-C exceptions. Keep the native
    // exception boundary inside the AppKit call stack instead of aborting.
    objc2::exception::catch(AssertUnwindSafe(call))
        .map_err(|exception| format!("The source app could not export the audio: {exception:?}"))
}

fn has_legacy_paths(pasteboard: &NSPasteboard) -> bool {
    // Same representation Wry collects. A normal Finder drop is imported by
    // the existing JS handler, never a second time through this bridge.
    pasteboard
        .propertyListForType(&NSString::from_str("NSFilenamesPboardType"))
        .and_then(|value| value.downcast::<NSArray>().ok())
        .is_some_and(|paths| !paths.is_empty())
}

fn receive_drop(app: &tauri::AppHandle, pasteboard: &NSPasteboard) {
    objc2::rc::autoreleasepool(|_| {
        // Use this native drag's pasteboard, never the general clipboard.
        let types: Vec<_> = pasteboard
            .types()
            .map(|types| types.iter().map(|t| t.to_string()).collect())
            .unwrap_or_default();
        let classes = NSArray::from_slice(&[NSFilePromiseReceiver::class()]);
        let promises = unsafe { pasteboard.readObjectsForClasses_options(&classes, None) };
        if let Some(promises) = promises.filter(|objects| !objects.is_empty()) {
            log::info!(
                "native_drop: {} promised exports, types={types:?}",
                promises.len()
            );
            if promises.len() > MAX_DROP_FILES {
                error(
                    app,
                    "native-drop",
                    "Audio import",
                    "Drop at most 64 files at once.".into(),
                );
                return;
            }
            let receivers: Vec<_> = promises
                .iter()
                .filter_map(|p| p.downcast::<NSFilePromiseReceiver>().ok())
                .collect();
            let file_count = receivers.iter().fold(0usize, |count, receiver| {
                count.saturating_add(promise_metadata(receiver).1)
            });
            if file_count > MAX_DROP_FILES {
                error(
                    app,
                    "native-drop",
                    "Audio import",
                    "Drop at most 64 files at once.".into(),
                );
                return;
            }
            let queue = NSOperationQueue::new();
            queue.setMaxConcurrentOperationCount(1);
            // AppKit requires every receiver in one drag to use the SAME
            // destination. The shared guard survives all export/import jobs.
            let dir = match temporary_export_dir() {
                Ok(dir) => dir,
                Err(failure) => {
                    error(app, "native-drop", "Audio import", failure);
                    return;
                }
            };
            for receiver in receivers {
                receive_promise(app, receiver, &queue, dir.clone());
            }
            return;
        }
        let paths = file_urls(&pasteboard);
        log::info!(
            "native_drop: {} modern file URLs, types={types:?}",
            paths.len()
        );
        if paths.is_empty() {
            // Don't turn ordinary HTML/text drags (e.g. pipeline chip
            // reordering) into audio-import errors.
            if types
                .iter()
                .any(|t| t.contains("file") || t.contains("audio"))
            {
                error(
                    app,
                    "native-drop",
                    "Audio import",
                    "The source app did not provide an audio file or file promise.".into(),
                );
            }
        } else if paths.len() > MAX_DROP_FILES {
            error(
                app,
                "native-drop",
                "Audio import",
                "Drop at most 64 files at once.".into(),
            );
        } else {
            for path in paths {
                queue_import(app.clone(), path, None);
            }
        }
    });
}

fn file_urls(pasteboard: &NSPasteboard) -> Vec<PathBuf> {
    let classes = NSArray::from_slice(&[NSURL::class()]);
    // SAFETY: NSURL implements NSPasteboardReading and there are no options.
    let urls = unsafe { pasteboard.readObjectsForClasses_options(&classes, None) };
    urls.into_iter()
        .flat_map(|urls| urls.to_vec())
        .filter_map(|object| {
            let url = object.downcast::<NSURL>().ok()?;
            if !url.isFileURL() {
                return None;
            }
            url.path().map(|path| PathBuf::from(path.to_string()))
        })
        .collect()
}

fn receive_promise(
    app: &tauri::AppHandle,
    receiver: objc2::rc::Retained<NSFilePromiseReceiver>,
    queue: &NSOperationQueue,
    dir: Arc<StagingDir>,
) {
    let id = format!("receive-{}", Uuid::new_v4());
    let (names, count) = promise_metadata(&receiver);
    if count > MAX_DROP_FILES {
        error(
            app,
            &id,
            "Audio import",
            "Drop at most 64 files at once.".into(),
        );
        return;
    }
    let title = if count == 1 {
        names.first().cloned().unwrap_or("Audio export".into())
    } else {
        format!("{count} audio exports")
    };
    let destination =
        NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&dir.0.to_string_lossy()), true);
    let remaining = Arc::new(AtomicUsize::new(count));
    emit(app, event(&id, "receiving", &title));
    log::info!("native_drop: receiving {count} promised file(s) into private temporary folder");
    ACTIVE_PROMISES.with(|active| {
        active
            .borrow_mut()
            .insert(id.clone(), (receiver.clone(), queue.retain()));
    });
    let callback_app = app.clone();
    let callback_id = id.clone();
    let callback_title = title.clone();
    let block = RcBlock::new(move |url: *mut NSURL, failure: *mut NSError| {
        let app = &callback_app;
        let id = &callback_id;
        let title = &callback_title;
        // Only paths and the temporary directory guard go to encoder workers;
        // no AppKit object crosses into the encoder task.
        objc2::rc::autoreleasepool(|_| {
            let _receiving = ReceivingFile {
                app: app.clone(),
                id: id.clone(),
                title: title.clone(),
                remaining: remaining.clone(),
            };
            if let Some(failure) = unsafe { failure.as_ref() } {
                log::warn!(
                    "native_drop: source export failed: {}",
                    failure.localizedDescription()
                );
                error(
                    &app,
                    &format!("failed-{}", Uuid::new_v4()),
                    &title,
                    failure.localizedDescription().to_string(),
                );
                return;
            }
            // An unsuccessful provider may pass nil despite the nonnull
            // annotation. Only dereference after checking error and URL.
            let Some(path) = (unsafe { url.as_ref() })
                .filter(|url| url.isFileURL())
                .and_then(|url| url.path())
            else {
                error(
                    &app,
                    &id,
                    &title,
                    "The export did not produce a local audio file.".into(),
                );
                return;
            };
            match received_path(&dir.0, Path::new(&path.to_string())) {
                Ok(path) => {
                    log::info!("native_drop: received promised audio; queuing streaming import");
                    queue_import(app.clone(), path, Some(dir.clone()));
                }
                Err(failure) => error(&app, &id, &title, failure),
            }
        });
    });
    // SAFETY: the destination is a private directory, options are empty, the
    // serial queue is a Foundation queue, and the callback uses no UI objects.
    let result = request_promise(&receiver, &destination, queue, &block);
    if let Err(failure) = result {
        ACTIVE_PROMISES.with(|active| {
            active.borrow_mut().remove(&id);
        });
        log::error!("native_drop: {failure}");
        error(app, &id, &title, failure);
        return;
    }
    log::info!("native_drop: requested promised export; main thread released");
}

fn request_promise(
    receiver: &NSFilePromiseReceiver,
    destination: &NSURL,
    queue: &NSOperationQueue,
    reader: &block2::DynBlock<dyn Fn(*mut NSURL, *mut NSError)>,
) -> Result<(), String> {
    native_call(|| unsafe {
        // Raw nullable URL signature matches the actual failure behavior.
        let _: () = msg_send![receiver,
            receivePromisedFilesAtDestination: destination,
            options: &*NSDictionary::<AnyObject, AnyObject>::new(),
            operationQueue: queue,
            reader: reader
        ];
    })
}

fn temporary_export_dir() -> Result<Arc<StagingDir>, String> {
    use std::os::unix::fs::DirBuilderExt;
    let path = std::env::temp_dir().join(format!("nbp-drop-{}", Uuid::new_v4()));
    // Atomic creation, private permissions, and a random name. Never share a
    // fixed /tmp filename or remove an arbitrary path supplied by a sender.
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .map_err(|e| format!("Cannot create temporary audio export folder: {e}"))?;
    Ok(Arc::new(StagingDir(path)))
}

fn received_path(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let path = path
        .canonicalize()
        .map_err(|e| format!("Cannot read exported audio: {e}"))?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err("The source app returned a file outside the export folder.".into());
    }
    Ok(path)
}

fn queue_import(app: tauri::AppHandle, path: PathBuf, source: Option<Arc<StagingDir>>) {
    let id = format!("import-{}", Uuid::new_v4());
    let title = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        error(
            &app,
            &id,
            &title,
            "The exported file is not a supported audio format.".into(),
        );
        return;
    }
    emit(&app, event(&id, "importing", &title));
    tauri::async_runtime::spawn(async move {
        let _source = source; // Keep the promised file until the copy/encode finishes; clean on every exit.
        match super::import_path(path).await {
            Ok(recording) => {
                let mut payload = event(&id, "complete", &recording.title);
                payload.recording_id = Some(recording.id);
                emit(&app, payload);
            }
            Err(failure) => error(&app, &id, &title, failure),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_hook_preserves_class_hover_handlers_and_original_drop() {
        use objc2::runtime::{ClassBuilder, NSObject};
        static FORWARDED: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C-unwind" fn original(_: &AnyObject, _: Sel, _: *mut AnyObject) -> Bool {
            FORWARDED.fetch_add(1, Ordering::SeqCst);
            Bool::NO
        }
        unsafe extern "C-unwind" fn enter(_: &AnyObject, _: Sel, _: *mut AnyObject) -> usize {
            1
        }

        let mut builder = ClassBuilder::new(c"NBPDropHookRegression", NSObject::class()).unwrap();
        unsafe {
            builder.add_method(
                sel!(performDragOperation:),
                original as unsafe extern "C-unwind" fn(_, _, _) -> _,
            );
            builder.add_method(
                sel!(draggingEntered:),
                enter as unsafe extern "C-unwind" fn(_, _, _) -> _,
            );
        }
        let class = builder.register();
        let object: objc2::rc::Retained<AnyObject> = unsafe { msg_send![class, new] };
        let size = class.instance_size();
        let enter_imp = class
            .instance_method(sel!(draggingEntered:))
            .unwrap()
            .implementation();
        install_drop_hook(class);
        assert_eq!(object.class(), class);
        assert_eq!(object.class().instance_size(), size);
        assert!(std::ptr::fn_addr_eq(
            enter_imp,
            class
                .instance_method(sel!(draggingEntered:))
                .unwrap()
                .implementation(),
        ));
        let accepted: Bool =
            unsafe { msg_send![&*object, performDragOperation: std::ptr::null_mut::<AnyObject>()] };
        assert!(!accepted.as_bool());
        assert_eq!(FORWARDED.load(Ordering::SeqCst), 1);
        let operation: usize =
            unsafe { msg_send![&*object, draggingEntered: std::ptr::null_mut::<AnyObject>()] };
        assert_eq!(operation, 1);
        // A second installation must not replace the saved IMP with our hook.
        install_drop_hook(class);
        let _: Bool =
            unsafe { msg_send![&*object, performDragOperation: std::ptr::null_mut::<AnyObject>()] };
        assert_eq!(FORWARDED.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn appkit_rejection_outside_native_drop_is_an_error_not_an_abort() {
        objc2::rc::autoreleasepool(|_| {
            let dir = temporary_export_dir().unwrap();
            let destination = NSURL::fileURLWithPath_isDirectory(
                &NSString::from_str(&dir.0.to_string_lossy()),
                true,
            );
            let reader = RcBlock::new(|_: *mut NSURL, _: *mut NSError| {});
            let failure = request_promise(
                &NSFilePromiseReceiver::new(),
                &destination,
                &NSOperationQueue::new(),
                &reader,
            )
            .unwrap_err();
            assert!(failure.contains("can only be called during"), "{failure}");
            // The process survived the real AppKit exception; successful calls
            // still return their value unchanged.
            assert_eq!(native_call(|| 42).unwrap(), 42);
        });
    }

    #[test]
    fn missing_promise_metadata_is_not_a_panic() {
        objc2::rc::autoreleasepool(|_| {
            let receiver = NSFilePromiseReceiver::new();
            let (names, count) = promise_metadata(&receiver);
            assert!(names.is_empty());
            assert_eq!(count, 1);
        });
    }

    #[test]
    fn exports_use_private_system_temp_folders_and_clean_up_after_last_owner() {
        use std::os::unix::fs::PermissionsExt;
        let first = temporary_export_dir().unwrap();
        let second = temporary_export_dir().unwrap();
        assert_ne!(first.0, second.0);
        assert_eq!(first.0.parent().unwrap(), std::env::temp_dir());
        assert_eq!(
            std::fs::metadata(&first.0).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let path = first.0.clone();
        let worker = first.clone();
        drop(first);
        assert!(path.exists());
        drop(worker);
        assert!(!path.exists());
    }

    #[test]
    fn accepts_only_completed_files_in_private_export_folder() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("New Recording.m4a");
        std::fs::write(&file, b"audio fixture").unwrap();
        assert_eq!(
            received_path(root.path(), &file).unwrap(),
            file.canonicalize().unwrap()
        );
        assert!(received_path(root.path(), root.path()).is_err());
        assert!(received_path(root.path(), &root.path().join("missing.m4a")).is_err());
        let outside = tempfile::NamedTempFile::new().unwrap();
        assert!(received_path(root.path(), outside.path()).is_err());
        let link = root.path().join("escape.m4a");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        assert!(received_path(root.path(), &link).is_err());
    }

    #[test]
    fn reads_modern_file_urls_with_encoded_unicode_names() {
        objc2::rc::autoreleasepool(|_| {
            // Unique test-only board: never the user's general or drag board.
            let board = NSPasteboard::pasteboardWithUniqueName();
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("Запись 1.m4a");
            std::fs::write(&path, b"fixture").unwrap();
            let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
            let kind = NSString::from_str("public.file-url");
            unsafe {
                board.declareTypes_owner(&NSArray::from_slice(&[&*kind]), None);
            }
            assert!(board.setString_forType(&url.absoluteString().unwrap(), &kind));
            // macOS may synthesize compatibility types alongside file-url.
            assert!(
                board
                    .types()
                    .unwrap()
                    .iter()
                    .any(|t| t.to_string() == "public.file-url")
            );
            assert_eq!(file_urls(&board), vec![path]);
            board.clearContents();
        });
    }
}
