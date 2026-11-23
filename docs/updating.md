# In-app updates

- Startup and focus perform background checks only. They never install or restart.
- A known update is cached locally and remains visible offline and across views/restarts. No dismiss button.
- UPDATE streams the archive to a private temporary file (not a full in-memory buffer), verifies its Minisign signature, checks the app identity/version and Apple code signature, replaces the app, then restarts. Errors retain the banner and offer RETRY. Partial downloads are removed; downloads are limited to 512 MiB and 15 minutes.
- Recording, dictation, imports, transcription and pipeline jobs prevent installation. New native work cannot start while the update owns the installation gate.
- Development builds, mounted DMGs and App Translocation cannot install updates. Launch an installed app from Applications.
- No admin shell is used. If the installation directory is not writable, the update fails without touching the existing app.

## One-time bootstrap

0.7.8 and older only link to the release page; they cannot self-install. Install the first updater-enabled release manually once. Later releases update through the in-app banner.

## Release artifacts

`build.sh` signs and notarizes the app and DMG, creates `nbp.app.tar.gz` plus its updater signature, then publishes a draft GitHub release containing the DMG, archive, signature and `latest.json`. It verifies asset sizes before making the release public/latest. Published releases cannot be overwritten by this script.

The static manifest currently targets **darwin-aarch64** only, matching the existing Apple Silicon build pipeline. Endpoint: `https://github.com/skopanev/no-bullshit-pipeline/releases/latest/download/latest.json`.

The updater uses the same Minisign verifier as Tauri's updater, through its streaming API; legacy non-prehashed signatures are rejected. Verification is mandatory and independent of Apple code signing. Bundle replacement stages on the installation filesystem, verifies the new app and its Apple developer team before touching the old app, then uses macOS `RENAME_SWAP` to exchange the two bundles atomically. If swapping fails, the current installation remains untouched. No recording/user-data directories are replaced.

## Signing key custody

The encrypted private updater key is outside the repo at `~/.config/nbp-release/nbp-updater.key` (directory 0700, key 0600). Its random password is in macOS Keychain, service `one.nbp.skk.updater-signing`, account `nbp-release`. Only the public key belongs in `tauri.conf.json`.

**Back up the encrypted key and its password securely and separately.** Losing either prevents signing updates trusted by existing clients. Do not regenerate the key during a release. CI can provide `TAURI_SIGNING_PRIVATE_KEY` (path or contents) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` through its secret store.

## Verification

`bun test tests`, `bun run lint`, `bun run check:js`, and `cargo check --manifest-path src-tauri/Cargo.toml --all-targets`. Unit tests never install over the running app or restart it. An end-to-end update requires two separately signed release versions installed in a disposable location, with no live recordings or background work; do not use a development binary to test self-installation.
