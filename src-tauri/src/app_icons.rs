//! Resolves an application icon (PNG, base64 data URL) from a bundle id.
//!
//! macOS can hand us the icon of any *installed* app via NSWorkspace, even when
//! the app isn't running — so an auto-recorded call keeps its icon in the list
//! long after the call (and the app) is gone. We only need the bundle id, which
//! `audio_process_detector` captures at detection time and stores in the
//! recording metadata.
//!
//! Two-tier cache:
//!   • in-memory `HashMap<bundle_id, data_url>` — avoids repeated FFI within a
//!     session (the list re-renders a lot).
//!   • on-disk `{data_dir}/app_icons/{sanitized}.png` — survives restarts so we
//!     don't re-render the NSImage every launch.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

use base64::Engine;

static MEM_CACHE: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);

/// Tauri command: returns a `data:image/png;base64,...` URL for the given bundle
/// id, or `None` if the app can't be resolved (frontend then shows a neutral
/// default glyph). Result is cached in memory and on disk.
#[tauri::command]
pub fn get_app_icon(app: tauri::AppHandle, bundle_id: String) -> Option<String> {
    log::info!("app_icons: get_app_icon called bundle={:?}", bundle_id);
    if bundle_id.is_empty() {
        return None;
    }

    // Drop any pre-v2 cache once per launch so users updating from a version
    // that wrote blank macOS 26 icons or helper-id placeholders re-render clean.
    purge_legacy_cache();

    // Electron helpers share one parent icon — key the cache by the normalized
    // id so `…slackmacgap.helper` and `…slackmacgap` collapse to one entry (and
    // any stale placeholder file at the helper key is bypassed).
    let cache_key = normalize_bundle_id(&bundle_id).to_string();

    // 1. Memory cache.
    if let Ok(mut guard) = MEM_CACHE.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        if let Some(hit) = map.get(&cache_key) {
            log::info!(
                "app_icons: MEM cache hit bundle={:?} present={}",
                bundle_id,
                hit.is_some()
            );
            return hit.clone();
        }
    }

    // 2. Disk cache.
    let cache_path = icon_cache_path(&cache_key);
    if let Some(ref path) = cache_path
        && path.exists()
        && let Ok(bytes) = std::fs::read(path)
    {
        log::info!(
            "app_icons: DISK cache hit bundle={:?} ({} bytes) path={}",
            bundle_id,
            bytes.len(),
            path.display()
        );
        let url = png_to_data_url(&bytes);
        store_mem(&cache_key, Some(url.clone()));
        return Some(url);
    }

    // 3. Resolve from the OS on the MAIN THREAD, render to PNG, persist.
    // AppKit icon drawing (lockFocus/drawInRect) must run on the main thread,
    // and rendering the dynamic `.icon` resources macOS 26 uses requires actual
    // drawing (no static bitmap rep to read).
    let png = {
        let (tx, rx) = std::sync::mpsc::channel();
        let bid = bundle_id.clone();
        if app
            .run_on_main_thread(move || {
                let _ = tx.send(render_icon_png(&bid));
            })
            .is_err()
        {
            log::warn!("app_icons: run_on_main_thread failed for {:?}", bundle_id);
            return None;
        }
        rx.recv().ok().flatten()
    };
    log::info!(
        "app_icons: resolve bundle={:?} -> {}",
        bundle_id,
        png.as_ref()
            .map(|b| format!("{} bytes", b.len()))
            .unwrap_or_else(|| "UNRESOLVED".to_string())
    );
    if let Some(ref bytes) = png
        && let Some(ref path) = cache_path
    {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, bytes);
    }
    let result = png.map(|b| png_to_data_url(&b));
    store_mem(&cache_key, result.clone());
    result
}

fn store_mem(bundle_id: &str, value: Option<String>) {
    if let Ok(mut guard) = MEM_CACHE.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        map.insert(bundle_id.to_string(), value);
    }
}

fn png_to_data_url(bytes: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:image/png;base64,{}", b64)
}

/// Electron/Chromium apps route audio through a helper process whose bundle id
/// is `<parent>.helper[.<role>]` (e.g. `com.tinyspeck.slackmacgap.helper`,
/// `com.google.Chrome.helper.renderer`). The helper `.app` only carries a
/// generic placeholder icon, so we strip the `.helper…` tail and resolve the
/// parent app, which has the real icon. Non-helper ids pass through unchanged.
fn normalize_bundle_id(bundle_id: &str) -> &str {
    // Safari's "Graphics and Media" process supplies this CoreAudio bundle
    // id for browser calls. It is not an installed .app, so use Safari's icon.
    // Normalize at lookup time to fix recordings already stored with this id.
    if bundle_id.eq_ignore_ascii_case("com.apple.WebKit.GPU") {
        return "com.apple.Safari";
    }
    match bundle_id.to_ascii_lowercase().find(".helper") {
        Some(idx) if idx > 0 => &bundle_id[..idx],
        _ => bundle_id,
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_bundle_id;

    #[test]
    fn safari_media_process_uses_safari_icon() {
        assert_eq!(
            normalize_bundle_id("com.apple.WebKit.GPU"),
            "com.apple.Safari"
        );
        assert_eq!(
            normalize_bundle_id("com.apple.webkit.gpu"),
            "com.apple.Safari"
        );
    }

    #[test]
    fn electron_helpers_still_use_parent_icons() {
        assert_eq!(
            normalize_bundle_id("com.google.Chrome.helper.renderer"),
            "com.google.Chrome"
        );
        assert_eq!(
            normalize_bundle_id("com.tinyspeck.slackmacgap.helper"),
            "com.tinyspeck.slackmacgap"
        );
    }

    #[test]
    fn other_bundle_ids_are_unchanged() {
        for id in [
            "com.apple.Safari",
            "com.apple.WebKit.Networking",
            "org.example.WebKitApp",
        ] {
            assert_eq!(normalize_bundle_id(id), id);
        }
    }
}

/// Versioned cache dir. Bumping the suffix invalidates every prior cache on the
/// next launch, so icons from older (buggy) versions are never served again.
const ICON_CACHE_DIR: &str = "app_icons_v2";

fn icon_cache_path(bundle_id: &str) -> Option<std::path::PathBuf> {
    let safe: String = bundle_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Some(
        crate::storage::get_data_dir()
            .join(ICON_CACHE_DIR)
            .join(format!("{}.png", safe)),
    )
}

/// Best-effort, once-per-process removal of the pre-v2 cache dir, whose contents
/// may include blank or placeholder icons written by older versions.
fn purge_legacy_cache() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let legacy = crate::storage::get_data_dir().join("app_icons");
        let _ = std::fs::remove_dir_all(legacy);
    });
}

// --- AppKit FFI --------------------------------------------------------------
//
// Raw msg_send (same style as audio_process_detector) to avoid pulling in the
// full objc2-app-kit typed surface. All AppKit calls return autoreleased
// objects, so everything runs inside an autoreleasepool.

/// NSBitmapImageFileType.png == 4 (AppKit, stable since 10.0).
const NS_BITMAP_FILE_TYPE_PNG: u64 = 4;

/// Target raster size for app icons — 64px covers a 16px display at up to @3x.
const ICON_RENDER_PX: f64 = 64.0;

// CoreGraphics geometry structs (NSPoint/NSSize/NSRect are these on 64-bit).
// Defined locally with Encode impls so we can pass NSSize/NSRect by value to
// AppKit drawing calls without pulling in objc2-app-kit / objc2-core-foundation.
#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}
unsafe impl objc2::encode::Encode for CGPoint {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}
unsafe impl objc2::encode::Encode for CGSize {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}
unsafe impl objc2::encode::Encode for CGRect {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
}
unsafe impl objc2::encode::RefEncode for CGRect {
    const ENCODING_REF: objc2::encode::Encoding =
        objc2::encode::Encoding::Pointer(&<Self as objc2::encode::Encode>::ENCODING);
}

/// Resolve + rasterize an app icon to PNG bytes. Must run on the main thread
/// (AppKit `lockFocus`/`drawInRect`). Exposed for the `render_icon` example
/// harness, which exercises this exact path without launching the full app.
pub fn render_icon_png(bundle_id: &str) -> Option<Vec<u8>> {
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject};

    autoreleasepool(|_| unsafe {
        let ns_workspace = AnyClass::get(c"NSWorkspace")?;
        let workspace: *mut AnyObject = objc2::msg_send![ns_workspace, sharedWorkspace];
        if workspace.is_null() {
            return None;
        }

        // Resolve the parent app first (Electron helpers have no real icon),
        // falling back to the raw id for the rare standalone `*.helper` app.
        let normalized = normalize_bundle_id(bundle_id);
        let path = app_path_for_bundle(workspace, normalized).or_else(|| {
            if normalized != bundle_id {
                app_path_for_bundle(workspace, bundle_id)
            } else {
                None
            }
        });
        let path = match path {
            Some(p) => p,
            None => {
                log::warn!(
                    "app_icons[{}]: app URL/path nil (not installed?)",
                    bundle_id
                );
                return None;
            }
        };

        // NSImage* icon = [workspace iconForFile:path];
        let icon: *mut AnyObject = objc2::msg_send![workspace, iconForFile: path];
        if icon.is_null() {
            log::warn!("app_icons[{}]: iconForFile nil", bundle_id);
            return None;
        }

        // Rasterize by DRAWING the icon into an offscreen bitmap. macOS 26
        // (Tahoe) apps that adopted the new dynamic `.icon` format (e.g. Slack)
        // produce an NSImage with no static bitmap/CGImage rep, so both
        // TIFFRepresentation and CGImageForProposedRect come back blank — only
        // actually drawing composites the live rendering. Old `.icns` apps
        // (e.g. Telegram) draw fine too. drawInRect/lockFocus require the main
        // thread, which the caller guarantees via run_on_main_thread.
        const NS_COMPOSITE_SOURCE_OVER: u64 = 2;
        let size = CGSize {
            width: ICON_RENDER_PX,
            height: ICON_RENDER_PX,
        };
        let dest = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size,
        };
        let zero = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: 0.0,
                height: 0.0,
            },
        };

        let _: () = objc2::msg_send![icon, setSize: size];

        // canvas = [[NSImage alloc] initWithSize:size]; [canvas lockFocus];
        let ns_image_cls = AnyClass::get(c"NSImage")?;
        let canvas: *mut AnyObject = objc2::msg_send![ns_image_cls, alloc];
        let canvas: *mut AnyObject = objc2::msg_send![canvas, initWithSize: size];
        if canvas.is_null() {
            log::warn!("app_icons[{}]: canvas alloc nil", bundle_id);
            return None;
        }
        let _: () = objc2::msg_send![canvas, lockFocus];
        let _: () = objc2::msg_send![
            icon,
            drawInRect: dest,
            fromRect: zero,
            operation: NS_COMPOSITE_SOURCE_OVER,
            fraction: 1.0f64
        ];
        // rep = [[NSBitmapImageRep alloc] initWithFocusedViewRect:dest];
        let ns_rep = AnyClass::get(c"NSBitmapImageRep")?;
        let rep_alloc: *mut AnyObject = objc2::msg_send![ns_rep, alloc];
        let rep: *mut AnyObject = objc2::msg_send![rep_alloc, initWithFocusedViewRect: dest];
        let _: () = objc2::msg_send![canvas, unlockFocus];
        if rep.is_null() {
            log::warn!("app_icons[{}]: bitmap rep nil after draw", bundle_id);
            return None;
        }

        // NSData* png = [rep representationUsingType:NSPNGFileType properties:@{}];
        let ns_dict = AnyClass::get(c"NSDictionary")?;
        let empty: *mut AnyObject = objc2::msg_send![ns_dict, dictionary];
        let png: *mut AnyObject = objc2::msg_send![
            rep,
            representationUsingType: NS_BITMAP_FILE_TYPE_PNG,
            properties: empty
        ];
        if png.is_null() {
            log::warn!("app_icons[{}]: PNG encode nil", bundle_id);
            return None;
        }

        nsdata_to_vec(png)
    })
}

/// Resolve a bundle id to its app bundle path (NSString*) via
/// `NSWorkspace.URLForApplicationWithBundleIdentifier`, or None if not installed.
unsafe fn app_path_for_bundle(
    workspace: *mut objc2::runtime::AnyObject,
    bundle_id: &str,
) -> Option<*mut objc2::runtime::AnyObject> {
    use objc2::runtime::AnyObject;
    unsafe {
        let bundle_ns = nsstring(bundle_id)?;
        let url: *mut AnyObject =
            objc2::msg_send![workspace, URLForApplicationWithBundleIdentifier: bundle_ns];
        if url.is_null() {
            return None;
        }
        let path: *mut AnyObject = objc2::msg_send![url, path];
        if path.is_null() { None } else { Some(path) }
    }
}

/// Build an autoreleased NSString from a Rust &str.
unsafe fn nsstring(s: &str) -> Option<*mut objc2::runtime::AnyObject> {
    use objc2::runtime::{AnyClass, AnyObject};
    let cls = AnyClass::get(c"NSString")?;
    let cstr = std::ffi::CString::new(s).ok()?;
    let obj: *mut AnyObject = objc2::msg_send![cls, stringWithUTF8String: cstr.as_ptr()];
    if obj.is_null() { None } else { Some(obj) }
}

/// Copy the bytes out of an NSData into an owned Vec (before the pool drains).
unsafe fn nsdata_to_vec(data: *mut objc2::runtime::AnyObject) -> Option<Vec<u8>> {
    let len: usize = objc2::msg_send![data, length];
    if len == 0 {
        return None;
    }
    let bytes: *const c_void = objc2::msg_send![data, bytes];
    if bytes.is_null() {
        return None;
    }
    let slice = unsafe { std::slice::from_raw_parts(bytes as *const u8, len) };
    Some(slice.to_vec())
}
