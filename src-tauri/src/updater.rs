//! Checks never install. Only an explicit main-window action can update an idle app.
use base64::Engine;
use futures_util::{Stream, StreamExt};
use minisign_verify::{PublicKey, Signature};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, WebviewWindow};
use tauri_plugin_updater::UpdaterExt;
use tokio::io::AsyncWriteExt;

#[derive(Serialize, Clone)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
    pub current: String,
}
#[derive(Serialize)]
pub struct UpdateStatus {
    pub busy: bool,
    pub installing: bool,
    pub unavailable_reason: Option<String>,
}

fn bundle_path(exe: &Path) -> Result<PathBuf, String> {
    let macos = exe.parent().ok_or("Cannot locate app")?;
    let contents = macos.parent().ok_or("Cannot locate app")?;
    let bundle = contents.parent().ok_or("Cannot locate app")?;
    if macos.file_name().is_none_or(|n| n != "MacOS")
        || contents.file_name().is_none_or(|n| n != "Contents")
        || bundle.extension().is_none_or(|n| n != "app")
    {
        return Err("In-app updates require an installed release, not a development build.".into());
    }
    if bundle.starts_with("/Volumes")
        || bundle
            .components()
            .any(|p| p.as_os_str() == "AppTranslocation")
    {
        return Err("Move nbp to Applications and launch it there before updating.".into());
    }
    Ok(bundle.to_owned())
}
fn installed_bundle() -> Result<PathBuf, String> {
    if cfg!(debug_assertions) {
        return Err("In-app updates are disabled in development builds.".into());
    }
    bundle_path(&std::env::current_exe().map_err(|e| e.to_string())?)
}
fn busy() -> bool {
    crate::update_gate::has_jobs()
        || !matches!(
            crate::capture_coordinator::snapshot(),
            crate::capture_coordinator::CaptureActivity::Idle
        )
}
#[tauri::command]
pub fn get_app_update_status() -> UpdateStatus {
    UpdateStatus {
        busy: busy(),
        installing: crate::update_gate::is_installing(),
        unavailable_reason: installed_bundle().err(),
    }
}
#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    let update = app
        .updater_builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?;
    Ok(update.map(|u| UpdateInfo {
        version: u.version,
        current: u.current_version,
        url: "https://github.com/skopanev/no-bullshit-pipeline/releases".into(),
    }))
}
#[derive(Clone, Serialize)]
struct Progress<'a> {
    request_id: &'a str,
    phase: &'a str,
    downloaded: u64,
    total: Option<u64>,
}
fn progress(app: &AppHandle, request_id: &str, phase: &str, downloaded: u64, total: Option<u64>) {
    let _ = app.emit_to(
        "main",
        "app-update-progress",
        Progress {
            request_id,
            phase,
            downloaded,
            total,
        },
    );
}
#[tauri::command]
pub async fn install_app_update(
    app: AppHandle,
    window: WebviewWindow,
    expected_version: String,
    request_id: String,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Updates must be started from the main window.".into());
    }
    let bundle = installed_bundle()?;
    let install = crate::update_gate::install()?;
    if busy() {
        return Err("Finish recording, dictation and background tasks before updating.".into());
    }
    progress(&app, &request_id, "checking", 0, None);
    let update = app
        .updater_builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("This update is no longer available. Reopen the app to check again.")?;
    if update.version != expected_version.trim_start_matches('v') {
        return Err(
            "The available version has changed. Reopen the app to refresh the update.".into(),
        );
    }
    let download = tempfile::Builder::new()
        .prefix("nbp-update-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let archive = download.path().join("update.tar.gz");
    let pubkey = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|c| c.get("pubkey"))
        .and_then(|v| v.as_str())
        .ok_or("Updater public key is missing")?
        .to_owned();
    let mut last = Instant::now() - Duration::from_secs(1);
    download_signed(
        &update,
        &pubkey,
        &archive,
        |downloaded, total| {
            if last.elapsed() >= Duration::from_millis(150) {
                progress(&app, &request_id, "downloading", downloaded, total);
                last = Instant::now();
            }
        },
        || progress(&app, &request_id, "verifying", 0, None),
    )
    .await
    .map_err(|e| e.to_string())?;
    // Streaming verification completed before any archive is extracted.
    progress(&app, &request_id, "installing", 0, None);
    let version = update.version;
    let _install = tauri::async_runtime::spawn_blocking(move || {
        let _download = download;
        replace_bundle(&bundle, &archive, &version)?;
        Ok::<_, String>(install)
    })
    .await
    .map_err(|e| e.to_string())??;
    progress(&app, &request_id, "restarting", 0, None);
    app.restart();
}

const MAX_UPDATE_BYTES: u64 = 512 * 1024 * 1024;

fn decode_signed_text(value: &str) -> Result<String, String> {
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(value.trim())
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

// Tauri uses base64-wrapped Minisign signatures. Use the same verifier with
// its prehashed streaming API, never a Vec containing the complete archive.
async fn download_signed(
    update: &tauri_plugin_updater::Update,
    pubkey: &str,
    archive: &Path,
    on_chunk: impl FnMut(u64, Option<u64>),
    on_verify: impl FnOnce(),
) -> Result<(), String> {
    let response = reqwest::Client::builder()
        .https_only(true)
        .timeout(Duration::from_secs(15 * 60))
        .user_agent(concat!("nbp/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?
        .get(update.download_url.clone())
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let total = response.content_length();
    write_verified_stream(
        response.bytes_stream(),
        total,
        pubkey,
        &update.signature,
        archive,
        on_chunk,
        on_verify,
    )
    .await
}

async fn write_verified_stream<S, B, E>(
    mut stream: S,
    total: Option<u64>,
    pubkey: &str,
    signature: &str,
    archive: &Path,
    mut on_chunk: impl FnMut(u64, Option<u64>),
    on_verify: impl FnOnce(),
) -> Result<(), String>
where
    S: Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
    E: std::fmt::Display,
{
    let public_key = PublicKey::decode(&decode_signed_text(pubkey)?).map_err(|e| e.to_string())?;
    let signature =
        Signature::decode(&decode_signed_text(signature)?).map_err(|e| e.to_string())?;
    let mut verifier = public_key
        .verify_stream(&signature)
        .map_err(|e| e.to_string())?;
    if total.is_some_and(|size| size > MAX_UPDATE_BYTES) {
        return Err("Update archive exceeds the size limit.".into());
    }
    let mut file = tokio::fs::File::create(archive)
        .await
        .map_err(|e| e.to_string())?;
    let mut downloaded = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        let chunk = chunk.as_ref();
        downloaded = downloaded
            .checked_add(chunk.len() as u64)
            .ok_or("Update size overflow")?;
        if downloaded > MAX_UPDATE_BYTES {
            return Err("Update archive exceeds the size limit.".into());
        }
        file.write_all(chunk).await.map_err(|e| e.to_string())?;
        verifier.update(chunk);
        on_chunk(downloaded, total);
    }
    file.flush().await.map_err(|e| e.to_string())?;
    file.sync_all().await.map_err(|e| e.to_string())?;
    if total.is_some_and(|size| size != downloaded) {
        return Err("Incomplete update download.".into());
    }
    on_verify();
    verifier
        .finalize()
        .map_err(|e| format!("Update signature verification failed: {e}"))?;
    Ok(())
}
fn run(command: &mut Command, context: &str) -> Result<String, String> {
    let output = command.output().map_err(|e| format!("{context}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{context}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
// Same-filesystem staging, verified before an atomic directory swap. On swap
// failure the current app is untouched. No privileged shell or admin prompt.
fn replace_bundle(bundle: &Path, archive: &Path, version: &str) -> Result<(), String> {
    let parent = bundle.parent().ok_or("Invalid installation path")?;
    let staging = tempfile::Builder::new()
        .prefix(".nbp-update-")
        .tempdir_in(parent)
        .map_err(|e| format!("Cannot write to the installation folder: {e}"))?;
    let extracted = staging.path().join("unpacked");
    std::fs::create_dir(&extracted).map_err(|e| e.to_string())?;
    run(
        Command::new("/usr/bin/tar")
            .arg("-xzf")
            .arg(archive)
            .arg("-C")
            .arg(&extracted),
        "Cannot extract update",
    )?;
    let new_bundle = extracted.join("nbp.app");
    let plist = new_bundle.join("Contents/Info.plist");
    let identifier = run(
        Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", "Print :CFBundleIdentifier"])
            .arg(&plist),
        "Cannot read update identity",
    )?;
    let actual_version = run(
        Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", "Print :CFBundleShortVersionString"])
            .arg(&plist),
        "Cannot read update version",
    )?;
    if identifier != "one.nbp.skk" || actual_version != version {
        return Err("The signed update has an unexpected app identity or version.".into());
    }
    run(
        Command::new("/usr/bin/codesign")
            .args([
                "--verify",
                "--deep",
                "--strict",
                "-R=anchor apple generic and certificate leaf[subject.OU] = \"Z499WGKJW6\"",
            ])
            .arg(&new_bundle),
        "Update code signature is invalid",
    )?;
    swap_bundles(&new_bundle, bundle)?;
    // The old bundle is now inside staging and can be removed safely.
    Ok(())
}

#[cfg(target_os = "macos")]
fn swap_bundles(new_bundle: &Path, installed: &Path) -> Result<(), String> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let source = CString::new(new_bundle.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let destination = CString::new(installed.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: both C strings are valid for this call. RENAME_SWAP atomically
    // exchanges existing paths on one filesystem, without a missing-app gap.
    if unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_SWAP) } != 0 {
        return Err(format!(
            "Cannot replace app; current installation is unchanged: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn swap_bundles(_new_bundle: &Path, _installed: &Path) -> Result<(), String> {
    Err("In-app installation is currently supported only on macOS.".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    // Public prehashed Minisign test vector from minisign-verify (ISC license).
    fn signing_fixture() -> (String, String) {
        let key = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        let signature = "untrusted comment: signature from minisign secret key\nRUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\ntrusted comment: timestamp:1556193335\tfile:test\ny/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";
        let encode = |text: &str| base64::engine::general_purpose::STANDARD.encode(text);
        (
            encode(&format!("untrusted comment: test key\n{key}\n")),
            encode(signature),
        )
    }
    #[tokio::test]
    async fn signed_download_stream_accepts_only_complete_authentic_bytes() {
        let (key, signature) = signing_fixture();
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("download");
        let chunks = vec![Ok::<_, String>(&b"te"[..]), Ok(&b"st"[..])];
        let mut progress = vec![];
        let mut verified = false;
        write_verified_stream(
            futures_util::stream::iter(chunks),
            Some(4),
            &key,
            &signature,
            &archive,
            |bytes, _| progress.push(bytes),
            || verified = true,
        )
        .await
        .unwrap();
        assert_eq!(progress, vec![2, 4]);
        assert!(verified);
        assert_eq!(std::fs::read(&archive).unwrap(), b"test");
        let corrupt = vec![Ok::<_, String>(&b"evil"[..])];
        assert!(
            write_verified_stream(
                futures_util::stream::iter(corrupt),
                Some(4),
                &key,
                &signature,
                &archive,
                |_, _| {},
                || {}
            )
            .await
            .unwrap_err()
            .contains("signature verification failed")
        );
        let truncated = vec![Ok::<_, String>(&b"te"[..])];
        assert!(
            write_verified_stream(
                futures_util::stream::iter(truncated),
                Some(4),
                &key,
                &signature,
                &archive,
                |_, _| {},
                || {}
            )
            .await
            .unwrap_err()
            .contains("Incomplete")
        );
        let failed = vec![Ok(&b"te"[..]), Err("network stopped")];
        assert!(
            write_verified_stream(
                futures_util::stream::iter(failed),
                None,
                &key,
                &signature,
                &archive,
                |_, _| {},
                || {}
            )
            .await
            .unwrap_err()
            .contains("network stopped")
        );
        let empty = futures_util::stream::iter(Vec::<Result<&[u8], String>>::new());
        assert!(
            write_verified_stream(
                empty,
                Some(MAX_UPDATE_BYTES + 1),
                &key,
                &signature,
                &archive,
                |_, _| {},
                || {}
            )
            .await
            .unwrap_err()
            .contains("size limit")
        );
    }
    #[test]
    fn configured_public_key_is_valid_and_encrypted_private_key_is_not_embedded() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let key = config["plugins"]["updater"]["pubkey"].as_str().unwrap();
        PublicKey::decode(&decode_signed_text(key).unwrap()).unwrap();
        assert!(!include_str!("../tauri.conf.json").contains("private_key"));
    }
    #[test]
    fn only_real_bundles_outside_disk_images_can_install() {
        assert_eq!(
            bundle_path(Path::new("/Applications/nbp.app/Contents/MacOS/nbp")).unwrap(),
            Path::new("/Applications/nbp.app")
        );
        for path in [
            "/tmp/target/debug/nbp",
            "/Volumes/nbp/nbp.app/Contents/MacOS/nbp",
            "/private/var/folders/x/AppTranslocation/uuid/d/nbp.app/Contents/MacOS/nbp",
        ] {
            assert!(bundle_path(Path::new(path)).is_err(), "{path}");
        }
    }
    #[test]
    fn corrupt_archive_does_not_touch_installed_app() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("nbp.app");
        std::fs::create_dir(&bundle).unwrap();
        std::fs::write(bundle.join("sentinel"), "old app").unwrap();
        let archive = temp.path().join("invalid.tar.gz");
        std::fs::write(&archive, "invalid archive").unwrap();
        assert!(replace_bundle(&bundle, &archive, "0.7.9").is_err());
        assert_eq!(
            std::fs::read_to_string(bundle.join("sentinel")).unwrap(),
            "old app"
        );
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 2);
    }
    #[test]
    #[ignore = "requires NBP_TEST_UPDATE_BUNDLE pointing to an existing signed release; never launches it"]
    fn signed_release_replaces_only_an_isolated_dummy_installation() {
        let source = PathBuf::from(
            std::env::var_os("NBP_TEST_UPDATE_BUNDLE").expect("Signed release fixture required"),
        )
        .canonicalize()
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("fixture.tar.gz");
        run(
            Command::new("/usr/bin/tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(source.parent().unwrap())
                .arg(source.file_name().unwrap()),
            "Cannot package fixture",
        )
        .unwrap();
        let version = run(
            Command::new("/usr/libexec/PlistBuddy")
                .args(["-c", "Print :CFBundleShortVersionString"])
                .arg(source.join("Contents/Info.plist")),
            "Cannot read fixture version",
        )
        .unwrap();
        let installed = temp.path().join("nbp.app");
        std::fs::create_dir(&installed).unwrap();
        std::fs::write(installed.join("sentinel"), "previous installation").unwrap();
        assert!(
            replace_bundle(&installed, &archive, "999.0.0")
                .unwrap_err()
                .contains("unexpected app identity or version")
        );
        assert!(installed.join("sentinel").is_file());
        replace_bundle(&installed, &archive, &version).unwrap();
        assert!(!installed.join("sentinel").exists());
        assert!(installed.join("Contents/MacOS/nbp").is_file());
        assert!(source.join("Contents/MacOS/nbp").is_file());
    }
    #[test]
    #[cfg(target_os = "macos")]
    fn replacement_is_atomic_and_failure_preserves_current_app() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("installed.app");
        let new = temp.path().join("new.app");
        std::fs::create_dir(&old).unwrap();
        std::fs::create_dir(&new).unwrap();
        std::fs::write(old.join("version"), "old").unwrap();
        std::fs::write(new.join("version"), "new").unwrap();
        swap_bundles(&new, &old).unwrap();
        assert_eq!(std::fs::read_to_string(old.join("version")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(new.join("version")).unwrap(), "old");
        assert!(swap_bundles(&temp.path().join("missing.app"), &old).is_err());
        assert_eq!(std::fs::read_to_string(old.join("version")).unwrap(), "new");
    }
}
