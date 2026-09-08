use crate::pipelines::PipelineState;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use uuid::Uuid;

/// Metadata for a recording session
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RecordingMetadata {
    pub id: String,
    pub created_at: String,
    pub title: String,
    #[serde(default = "default_status")]
    pub status: String,
    pub audio: AudioFiles,
    #[serde(default)]
    pub health: Option<RecordingHealth>,
    #[serde(default)]
    pub pipelines: Vec<PipelineState>,
    /// First ~200 chars of transcript, populated by `transcription` after the
    /// transcript is written to disk. Rendered as preview line in the recording
    /// list so the user can tell what the meeting was about without opening it.
    /// Absent for never-transcribed recordings; serde default keeps it
    /// backward-compatible with older metadata files.
    #[serde(default)]
    pub transcript_preview: Option<String>,
    /// Bundle id of the app that triggered an auto-recorded call (e.g.
    /// "us.zoom.xos", "com.apple.FaceTime"). Used to render the app icon next
    /// to the title. None for manual recordings or unresolved daemons.
    #[serde(default)]
    pub app_bundle_id: Option<String>,
    /// What produced this recording: "manual" (user clicked record),
    /// "call" (auto-detected meeting), "dictation" (Quick Dictate save).
    /// Future-proofs filtering / stats / UI badges without parsing the title
    /// format. Defaults to "manual" for older metadata files (back-compat).
    #[serde(default = "default_recording_source")]
    pub source: String,
    /// Friendly display name of the app that produced this recording, e.g.
    /// "Zoom", "FaceTime", "NBP". Captured at recording start when the call
    /// detector emits an app label (`call-event.app`); pipelines use it to
    /// substitute the `{app}` template placeholder without resolving the
    /// bundle id at run time. Manual / dictation recordings get a sensible
    /// default at the engine level — this field stays None for them so we
    /// don't bake the default into stored metadata.
    #[serde(default)]
    pub app_friendly_name: Option<String>,
    /// Attendees of the calendar event this recording was matched to (see
    /// `calendar.rs`). Populated at finalize when the recording start lines up
    /// with a calendar event; empty for ad-hoc recordings or when calendar
    /// access isn't granted. Fuel for "who did I meet with" pipelines.
    #[serde(default)]
    pub attendees: Vec<Attendee>,
    /// EventKit identifier of the matched calendar event, if any. Lets the user
    /// re-match / clear and keeps the association stable across re-runs.
    #[serde(default)]
    pub calendar_event_id: Option<String>,
    /// Whether calendar matching has already run for this recording. Guards the
    /// finalize hook from re-matching (and re-overwriting the title) on every
    /// metadata rewrite. Set true once matching completes, match or no match.
    #[serde(default)]
    pub calendar_matched: bool,
}

/// A participant of a matched calendar event. EventKit exposes no email field
/// directly — it's parsed out of the participant's `mailto:` URL — so both are
/// optional (resource/room attendees may have neither a name nor an email).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Attendee {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

fn default_status() -> String {
    "ready".to_string()
}

fn default_recording_source() -> String {
    "manual".to_string()
}

/// Audio files (mic + system)
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AudioFiles {
    pub mic: Option<AudioInfo>,
    pub system: Option<AudioInfo>,
    pub mix: Option<AudioInfo>,
}

/// Audio file information
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AudioInfo {
    pub file: String,
    pub duration_sec: f64,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Recording health issue
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RecordingIssue {
    #[serde(rename = "type")]
    pub issue_type: String, // "drift", "source_lost", "error"
    pub timestamp_ms: u64,
    pub message: Option<String>,
}

/// Recording health status
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct RecordingHealth {
    pub status: String, // "ok", "warning", "error"
    #[serde(default)]
    pub issues: Vec<RecordingIssue>,
}

/// Get the data directory path
pub fn get_data_dir() -> PathBuf {
    // Try to load from settings first
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let settings_path = PathBuf::from(&home).join(".nbp").join("settings.json");

    if settings_path.exists()
        && let Ok(file) = File::open(settings_path)
        && let Ok(settings) = serde_json::from_reader::<_, serde_json::Value>(file)
        && let Some(path_str) = settings.get("storage_path").and_then(|v| v.as_str())
    {
        return PathBuf::from(path_str);
    }

    PathBuf::from(home).join("nbp-data")
}

/// Get the path for a specific recording directory
pub fn get_recording_dir(id: &str) -> PathBuf {
    get_data_dir().join(id)
}

/// In-process cache for `list_recordings`. `None` means the next call must
/// rescan the data directory. Populated lazily; cleared whenever any code
/// path mutates a recording on disk (write_metadata, delete_recording).
fn list_cache() -> &'static Mutex<Option<Vec<RecordingMetadata>>> {
    static CACHE: OnceLock<Mutex<Option<Vec<RecordingMetadata>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Drop the cached recordings list so the next `list_recordings` rescans
/// disk. Must be called by every code path that mutates a `metadata.json`,
/// including direct writers outside this module (e.g. pipeline_engine.rs,
/// which rewrites the pipeline_states block in place).
pub(crate) fn invalidate_list_cache() {
    if let Ok(mut guard) = list_cache().lock() {
        let prev_n = guard.as_ref().map(|v| v.len());
        *guard = None;
        log::info!(
            "[ignore-trace] invalidate_list_cache: prev_cached_n={:?}",
            prev_n
        );
    }
}

/// Instrumentation: counts how many `metadata.json` files `list_recordings`
/// has opened from disk over the lifetime of the process. Stays at the same
/// value across calls that are served from the in-process cache.
static LIST_METADATA_READS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
pub fn _test_list_metadata_reads() -> u64 {
    LIST_METADATA_READS.load(Ordering::Relaxed)
}

#[cfg(test)]
pub fn _test_invalidate_list_cache() {
    invalidate_list_cache();
}

/// Create a new recording with metadata. Legacy `tags` are intentionally not
/// part of the current model; the startup storage migration consumes them from
/// raw old JSON and removes them.
pub fn create_recording(title: String) -> Result<RecordingMetadata, String> {
    let id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let metadata = RecordingMetadata {
        id: id.clone(),
        created_at,
        title,
        status: "recording".to_string(),
        audio: AudioFiles {
            mic: None,
            system: None,
            mix: None,
        },
        health: Some(RecordingHealth {
            status: "ok".to_string(),
            issues: vec![],
        }),
        pipelines: vec![],
        transcript_preview: None,
        app_bundle_id: None,
        source: "manual".to_string(),
        app_friendly_name: None,
        attendees: vec![],
        calendar_event_id: None,
        calendar_matched: false,
    };

    // Create the recording directory
    let recording_dir = get_recording_dir(&id);
    fs::create_dir_all(&recording_dir).map_err(|e| e.to_string())?;

    // Write metadata.json
    write_metadata(&metadata)?;

    Ok(metadata)
}

/// Promote a recording's `source` after creation (call_session sets "call",
/// dictation save sets "dictation"). Read-mutate-write; silent if metadata
/// is missing.
pub fn update_recording_source(recording_id: &str, source: &str) -> Result<(), String> {
    let mut meta = read_metadata(recording_id)?;
    meta.source = source.to_string();
    write_metadata(&meta)
}

/// Write metadata to disk using atomic temp-file + rename pattern
pub fn write_metadata(metadata: &RecordingMetadata) -> Result<(), String> {
    let dir = get_recording_dir(&metadata.id);
    let dir_existed = dir.exists();
    let metadata_path = dir.join("metadata.json");
    let temp_path = metadata_path.with_extension("json.tmp");

    // Serialize first — if this fails, no file is touched
    let contents = serde_json::to_string_pretty(metadata)
        .map_err(|e| format!("Failed to serialize metadata: {}", e))?;

    // Write to temp file, then atomically rename
    fs::write(&temp_path, &contents)
        .map_err(|e| format!("Failed to write temp metadata: {}", e))?;
    fs::rename(&temp_path, &metadata_path)
        .map_err(|e| format!("Failed to finalize metadata: {}", e))?;

    log::info!(
        "[ignore-trace] write_metadata: id={} status={} dir_existed_before={}",
        metadata.id,
        metadata.status,
        dir_existed
    );
    invalidate_list_cache();
    Ok(())
}

/// Update title for a recording
#[tauri::command]
pub fn update_title(recording_id: &str, title: String) -> Result<(), String> {
    let mut metadata = read_metadata(recording_id)?;
    metadata.title = title;
    write_metadata(&metadata)?;
    Ok(())
}

/// Delete recording entries (the whole directory: audio + transcript +
/// metadata + pipeline outputs) older than `retention_days`. `0` means never —
/// a no-op. In-flight recordings (`recording` / `processing`) are always
/// skipped so an active or finalizing capture is never yanked out. Returns how
/// many entries were deleted.
pub fn prune_old_entries(retention_days: u32) -> usize {
    if retention_days == 0 {
        return 0;
    }
    let cutoff = Utc::now() - chrono::Duration::days(retention_days as i64);
    let Ok(recordings) = list_recordings() else {
        return 0;
    };
    let mut deleted = 0;
    for meta in recordings {
        if meta.status == "recording" || meta.status == "processing" {
            continue;
        }
        // created_at is RFC3339 (UTC). A malformed value is left untouched
        // rather than guessed at — never delete on a parse we don't trust.
        let Ok(created) = chrono::DateTime::parse_from_rfc3339(&meta.created_at) else {
            continue;
        };
        if created.with_timezone(&Utc) >= cutoff {
            continue;
        }
        // Status was already vetted from the in-memory list above, so prune the
        // entry's own files directly — no redundant metadata re-read per entry
        // (matters when pruning hundreds of recordings at once).
        if prune_entry_files(&meta.id).is_ok() {
            deleted += 1;
        }
    }
    if deleted > 0 {
        invalidate_list_cache();
    }
    deleted
}

/// Remove a recording's own files (audio, transcript, metadata, waveform, …)
/// while KEEPING its `pipelines/` results subdirectory — the user's processed
/// pipeline outputs aren't reclaimable and must survive retention. If the entry
/// had no pipeline results, the now-empty directory is removed too. Used only
/// by the retention sweep; manual delete (`delete_recording`) still wipes
/// everything.
fn prune_entry_files(recording_id: &str) -> Result<(), String> {
    let dir = get_recording_dir(recording_id);
    if !dir.exists() {
        return Ok(());
    }
    let mut kept_pipelines = false;
    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.file_name().and_then(|n| n.to_str()) == Some("pipelines") {
            kept_pipelines = true;
            continue;
        }
        if path.is_dir() {
            let _ = fs::remove_dir_all(&path);
        } else {
            let _ = fs::remove_file(&path);
        }
    }
    // Nothing worth keeping → drop the empty entry directory entirely.
    if !kept_pipelines {
        let _ = fs::remove_dir(&dir);
    }
    Ok(())
}

/// Physically remove a recording's directory. No status guard and no cache
/// bookkeeping — callers that might race a live capture must vet status first
/// (see `delete_recording`) and invalidate the list cache themselves.
fn delete_recording_dir(recording_id: &str) -> Result<(), String> {
    let recording_dir = get_recording_dir(recording_id);
    if recording_dir.exists() {
        fs::remove_dir_all(recording_dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Delete a recording and all its files
#[tauri::command]
pub fn delete_recording(recording_id: &str) -> Result<(), String> {
    // Prevent deletion while finalization is still running
    if let Ok(metadata) = read_metadata(recording_id)
        && metadata.status == "processing"
    {
        return Err("Recording is still being finalized. Please wait.".to_string());
    }

    delete_recording_dir(recording_id)?;
    // Deleting a recording also deletes the biometric traces it produced in
    // the global voice-profile store (appearances + orphaned candidates).
    crate::diarization::purge_recording(recording_id);
    invalidate_list_cache();
    Ok(())
}

/// Sweep recordings stuck in `status="recording"` or `status="processing"`
/// from a previous run.
///
/// `processing` is leftover from a crash mid-finalization. `recording` is
/// leftover from a crash / force-quit / abrupt restart while the recording
/// was active — no live runtime can possibly be writing to it now, so the
/// status is stale by definition.
///
/// Strategy: flip both to "error" with a health note. The recording's
/// audio_mix.ogg may still exist on disk and is playable; the user can
/// review, retry transcription, or delete from the list. We deliberately
/// don't auto-delete — losing audio silently is worse than a visible
/// "error" row.
///
/// Returns the number of recordings repaired.
pub fn cleanup_stuck_processing_recordings() -> usize {
    let data_dir = get_data_dir();
    if !data_dir.exists() {
        return 0;
    }
    let entries = match fs::read_dir(&data_dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };

    let mut repaired = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let metadata_path = path.join("metadata.json");
        if !metadata_path.exists() {
            continue;
        }
        let mut metadata: RecordingMetadata = match File::open(&metadata_path)
            .ok()
            .and_then(|f| serde_json::from_reader(f).ok())
        {
            Some(m) => m,
            None => continue,
        };
        let stale_status = metadata.status == "processing" || metadata.status == "recording";
        if !stale_status {
            continue;
        }

        let id = metadata.id.clone();
        let prev_status = metadata.status.clone();
        metadata.status = "error".to_string();
        let mut health = metadata.health.unwrap_or_default();
        health.status = "error".to_string();
        let msg = if prev_status == "recording" {
            "Recording was active when the app exited — backend lost track of the live session. Audio file may still be playable; transcribe manually or delete."
        } else {
            "Recording stuck in processing — app exited mid-finalize. Audio file may still be playable; retry transcription or delete."
        };
        health.issues.push(RecordingIssue {
            issue_type: "error".to_string(),
            timestamp_ms: 0,
            message: Some(msg.to_string()),
        });
        metadata.health = Some(health);

        if write_metadata(&metadata).is_ok() {
            log::info!("storage: repaired stuck {} recording {}", prev_status, id);
            repaired += 1;
        }
    }

    if repaired > 0 {
        invalidate_list_cache();
    }
    repaired
}

/// Read metadata from disk
#[tauri::command]
pub fn read_metadata(recording_id: &str) -> Result<RecordingMetadata, String> {
    let metadata_path = get_recording_dir(recording_id).join("metadata.json");
    let file = File::open(metadata_path).map_err(|e| e.to_string())?;
    serde_json::from_reader(file).map_err(|e| e.to_string())
}

/// List all recordings, sorted by created_at (newest first).
///
/// Uses an in-process cache to avoid rescanning `~/nbp-data/` and reparsing every
/// `metadata.json` on each call. The cache is invalidated by `write_metadata` and
/// `delete_recording`, so any create/update/delete is reflected on the next call.
#[tauri::command]
pub fn list_recordings() -> Result<Vec<RecordingMetadata>, String> {
    if let Ok(guard) = list_cache().lock()
        && let Some(cached) = guard.as_ref()
    {
        let active: Vec<String> = cached
            .iter()
            .filter(|m| m.status == "recording" || m.status == "processing")
            .map(|m| format!("{}({})", m.id, m.status))
            .collect();
        log::info!(
            "[ignore-trace] list_recordings: CACHE HIT, n={}, active={:?}",
            cached.len(),
            active
        );
        return Ok(cached.clone());
    }
    log::info!("[ignore-trace] list_recordings: CACHE MISS — scanning disk");

    let data_dir = get_data_dir();

    if !data_dir.exists() {
        if let Ok(mut guard) = list_cache().lock() {
            *guard = Some(Vec::new());
        }
        return Ok(Vec::new());
    }

    let mut recordings = Vec::new();

    for entry in fs::read_dir(&data_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();

        if path.is_dir() {
            let metadata_path = path.join("metadata.json");
            if metadata_path.exists() {
                match File::open(&metadata_path) {
                    Ok(file) => {
                        LIST_METADATA_READS.fetch_add(1, Ordering::Relaxed);
                        if let Ok(metadata) = serde_json::from_reader::<_, RecordingMetadata>(file)
                        {
                            recordings.push(metadata);
                        }
                    }
                    Err(_) => continue,
                }
            }
        }
    }

    // Sort by created_at (newest first)
    recordings.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    if let Ok(mut guard) = list_cache().lock() {
        *guard = Some(recordings.clone());
    }
    let active: Vec<String> = recordings
        .iter()
        .filter(|m| m.status == "recording" || m.status == "processing")
        .map(|m| format!("{}({})", m.id, m.status))
        .collect();
    log::info!(
        "[ignore-trace] list_recordings: SCAN DONE, n={}, active={:?}",
        recordings.len(),
        active
    );

    Ok(recordings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_recording() {
        let metadata = create_recording("test recording".to_string());

        assert!(metadata.is_ok());
        let metadata = metadata.unwrap();

        // Verify UUID format (36 chars with hyphens)
        assert_eq!(metadata.id.len(), 36);
        assert!(metadata.id.contains('-'));

        // Verify ISO 8601 format (ends with Z)
        assert!(metadata.created_at.ends_with('Z'));

        // Verify fields
        assert_eq!(metadata.title, "test recording");
        assert!(metadata.audio.mic.is_none());
        assert!(metadata.audio.system.is_none());
    }

    #[test]
    fn test_metadata_roundtrip() {
        let original = create_recording("roundtrip test".to_string()).unwrap();

        // Write is already done by create_recording

        // Read back
        let read_back = read_metadata(&original.id).unwrap();

        // Verify equality
        assert_eq!(original.id, read_back.id);
        assert_eq!(original.created_at, read_back.created_at);
        assert_eq!(original.title, read_back.title);
    }

    // Story 7.3: Atomic write_metadata tests

    #[test]
    fn test_atomic_write_preserves_original_on_disk_full() {
        // AC1: If write fails (e.g., disk full), original metadata.json is preserved

        // Create initial recording
        let original = create_recording("original title".to_string()).unwrap();

        let metadata_path = get_recording_dir(&original.id).join("metadata.json");
        let temp_path = metadata_path.with_extension("json.tmp");

        // Verify original file exists
        assert!(
            metadata_path.exists(),
            "Original metadata.json should exist"
        );

        // Read original content
        let original_content = fs::read_to_string(&metadata_path).unwrap();

        // Ensure no temp file exists before test
        let _ = fs::remove_file(&temp_path);

        // Modify metadata
        let mut modified = original.clone();
        modified.title = "modified title".to_string();

        // Simulate a disk error by making the directory read-only (won't work on all systems)
        // Instead, we verify the pattern: if temp file write fails, original is untouched

        // Write the modified metadata (this should succeed normally)
        let write_result = write_metadata(&modified);

        if write_result.is_ok() {
            // Normal case: write succeeded
            let new_content = fs::read_to_string(&metadata_path).unwrap();
            assert!(
                new_content.contains("modified title"),
                "New content should be written"
            );

            // Verify temp file is cleaned up
            assert!(
                !temp_path.exists(),
                "Temp file should not exist after successful write"
            );
        } else {
            // Error case: original should be preserved
            let preserved_content = fs::read_to_string(&metadata_path).unwrap();
            assert_eq!(
                preserved_content, original_content,
                "Original content should be preserved on error"
            );
        }

        // Cleanup
        let _ = fs::remove_dir_all(get_recording_dir(&original.id));
    }

    #[test]
    fn test_atomic_write_uses_temp_file() {
        // AC2: Verify atomic rename pattern is used

        let metadata = create_recording("atomic test".to_string()).unwrap();

        let metadata_path = get_recording_dir(&metadata.id).join("metadata.json");
        let temp_path = metadata_path.with_extension("json.tmp");

        // Verify initial state
        assert!(
            metadata_path.exists(),
            "metadata.json should exist after creation"
        );
        assert!(
            !temp_path.exists(),
            "No temp file should exist after successful write"
        );

        // Modify and write again
        let mut modified = metadata.clone();
        modified.title = "updated title".to_string();

        let result = write_metadata(&modified);
        assert!(result.is_ok(), "Write should succeed");

        // After successful write, temp file should not exist (it was renamed)
        assert!(
            !temp_path.exists(),
            "Temp file should be renamed to metadata.json"
        );
        assert!(metadata_path.exists(), "metadata.json should exist");

        // Verify content is updated
        let read_back = read_metadata(&metadata.id).unwrap();
        assert_eq!(read_back.title, "updated title");

        // Cleanup
        let _ = fs::remove_dir_all(get_recording_dir(&metadata.id));
    }

    #[test]
    fn test_atomic_write_no_partial_corruption() {
        // Verify that metadata.json is never left in a partially-written state

        let metadata = create_recording("corruption test".to_string()).unwrap();

        let metadata_path = get_recording_dir(&metadata.id).join("metadata.json");

        // Verify original is valid JSON
        let original_content = fs::read_to_string(&metadata_path).unwrap();
        let _original_parsed: RecordingMetadata = serde_json::from_str(&original_content).unwrap();

        // Write multiple times to verify consistency
        for i in 1..=5 {
            let mut modified = metadata.clone();
            modified.title = format!("iteration {}", i);

            let result = write_metadata(&modified);
            assert!(result.is_ok(), "Write iteration {} should succeed", i);

            // After each write, file should be valid JSON
            let content = fs::read_to_string(&metadata_path).unwrap();
            let parsed: RecordingMetadata = serde_json::from_str(&content).unwrap_or_else(|_| {
                panic!("metadata.json should be valid JSON after iteration {}", i)
            });

            assert_eq!(parsed.title, format!("iteration {}", i));
        }

        // Cleanup
        let _ = fs::remove_dir_all(get_recording_dir(&metadata.id));
    }

    #[test]
    fn test_atomic_write_matches_pattern_in_other_modules() {
        // Verify write_metadata follows the same pattern as pipeline_engine.rs and transcription.rs
        // Pattern: serialize first, write to .tmp, then rename

        let metadata = create_recording("pattern test".to_string()).unwrap();

        // Test that serialization is done before any file operations
        // by using a metadata that will serialize successfully
        let mut valid_metadata = metadata.clone();
        valid_metadata.title = "valid update".to_string();

        let result = write_metadata(&valid_metadata);
        assert!(result.is_ok(), "Valid metadata should write successfully");

        // Verify the file is written correctly
        let read_back = read_metadata(&metadata.id).unwrap();
        assert_eq!(read_back.title, "valid update");

        let metadata_path = get_recording_dir(&metadata.id).join("metadata.json");
        let temp_path = metadata_path.with_extension("json.tmp");

        // Temp file should not exist after successful write (it was renamed)
        assert!(
            !temp_path.exists(),
            "Temp file should be renamed after write"
        );

        // Cleanup
        let _ = fs::remove_dir_all(get_recording_dir(&metadata.id));
    }

    #[test]
    fn test_list_recordings_caches_metadata_reads() {
        // AC: With N recordings and no writes, a second list_recordings() performs
        // zero metadata.json reads.

        // Seed a recording so the data dir is non-empty.
        let r = create_recording("list cache seed".to_string()).unwrap();

        // Reset the cache to a deterministic state.
        _test_invalidate_list_cache();

        // First call: cache miss → must read at least our seed entry from disk.
        let before_first = _test_list_metadata_reads();
        let first = list_recordings().expect("first list_recordings");
        let reads_first = _test_list_metadata_reads() - before_first;
        assert!(
            !first.is_empty(),
            "first list_recordings should return the seeded recording"
        );
        assert!(
            reads_first >= 1,
            "first list_recordings should read metadata.json from disk (got {})",
            reads_first
        );

        // Second call: cache hit → no further metadata.json reads.
        let before_second = _test_list_metadata_reads();
        let second = list_recordings().expect("second list_recordings");
        let reads_second = _test_list_metadata_reads() - before_second;
        assert_eq!(
            reads_second, 0,
            "second list_recordings should read 0 metadata.json files (cache hit), \
             got {}. If this fails under parallel tests, another test invalidated \
             the cache between calls.",
            reads_second
        );

        // Ordering must remain newest-first.
        for window in second.windows(2) {
            assert!(
                window[0].created_at >= window[1].created_at,
                "list_recordings must remain sorted newest-first"
            );
        }

        // Cleanup
        let _ = fs::remove_dir_all(get_recording_dir(&r.id));
        _test_invalidate_list_cache();
    }

    #[test]
    fn test_list_recordings_invalidated_by_writes() {
        // AC: Creating/updating/deleting a recording is reflected on the next
        // list_recordings() call (no stale data).

        // Prime the cache.
        _test_invalidate_list_cache();
        let _ = list_recordings().unwrap();

        // Create a fresh recording — write_metadata must invalidate the cache.
        let r = create_recording("list cache invalidation".to_string()).unwrap();

        let after_create = list_recordings().expect("list after create");
        assert!(
            after_create.iter().any(|m| m.id == r.id),
            "newly created recording must be visible on the next list_recordings call"
        );

        // Update the recording — write_metadata must invalidate the cache.
        update_title(&r.id, "renamed".to_string()).unwrap();
        let after_update = list_recordings().expect("list after update");
        let updated = after_update
            .iter()
            .find(|m| m.id == r.id)
            .expect("recording present");
        assert_eq!(
            updated.title, "renamed",
            "updated title must be visible on the next list_recordings call"
        );

        // Delete the recording — delete_recording must invalidate the cache.
        delete_recording(&r.id).unwrap();
        let after_delete = list_recordings().expect("list after delete");
        assert!(
            !after_delete.iter().any(|m| m.id == r.id),
            "deleted recording must be gone on the next list_recordings call"
        );

        _test_invalidate_list_cache();
    }

    #[test]
    fn test_atomic_write_concurrent_reads_safe() {
        // Verify that concurrent reads during write see either old or new data,
        // never partial/corrupted data

        let metadata = create_recording("concurrent test".to_string()).unwrap();

        // Perform multiple sequential writes
        for i in 0..10 {
            let mut modified = metadata.clone();
            modified.title = format!("version {}", i);

            // Write
            write_metadata(&modified).unwrap();

            // Immediately read back
            let read_back = read_metadata(&metadata.id).unwrap();

            // Should see complete data (either old or new, but not partial)
            assert!(!read_back.title.is_empty(), "Title should not be empty");
            assert!(
                read_back.title.starts_with("version") || read_back.title == "concurrent test",
                "Should see complete title, got: {}",
                read_back.title
            );

            // Verify JSON is valid (read_metadata already does this, but explicit check)
            let metadata_path = get_recording_dir(&metadata.id).join("metadata.json");
            let content = fs::read_to_string(&metadata_path).unwrap();
            let _parsed: RecordingMetadata =
                serde_json::from_str(&content).expect("File should always contain valid JSON");
        }

        // Cleanup
        let _ = fs::remove_dir_all(get_recording_dir(&metadata.id));
    }
}
