use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use crate::storage::{get_data_dir, read_metadata};

/// Source of the transcript
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptSource {
    Local,        // Local Whisper
    Fluidaudio,   // FluidAudio (local ASR + diarization)
    Openai,       // OpenAI Whisper-1 API
    Google,       // Google Gemini
    Anthropic,    // Anthropic Claude
    Apple,        // Apple SpeechAnalyzer (macOS 26+)
    #[serde(other)]
    Unknown,
}

/// Transcript metadata stored in YAML frontmatter
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TranscriptMetadata {
    pub source: TranscriptSource,
    pub model: String,
    pub created_at: String, // ISO 8601
    pub duration_sec: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// Parsed transcript with optional metadata
#[allow(dead_code)]
pub struct TranscriptData {
    pub metadata: Option<TranscriptMetadata>,
    pub content: String,
}

/// Read a transcript file, supporting both frontmatter and legacy formats
#[allow(dead_code)]
pub fn read_transcript(path: &Path) -> Result<TranscriptData, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("Failed to read transcript: {}", e))?;

    if raw.starts_with("---") {
        // Try to parse frontmatter
        let parts: Vec<&str> = raw.splitn(3, "---").collect();
        if parts.len() >= 3 {
            let frontmatter = parts[1].trim();
            let content = parts[2].trim();

            if let Ok(metadata) = serde_yaml::from_str::<TranscriptMetadata>(frontmatter) {
                return Ok(TranscriptData {
                    metadata: Some(metadata),
                    content: content.to_string(),
                });
            }
        }
    }

    // Legacy format - no frontmatter
    Ok(TranscriptData {
        metadata: None,
        content: raw,
    })
}

/// Write a transcript with frontmatter metadata
pub fn write_transcript_with_metadata(
    path: &Path,
    content: &str,
    metadata: &TranscriptMetadata,
) -> Result<(), String> {
    let frontmatter =
        serde_yaml::to_string(metadata).map_err(|e| format!("Failed to serialize metadata: {}", e))?;

    let file_content = format!("---\n{}---\n\n{}", frontmatter, content);

    // Atomic write
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, &file_content)
        .map_err(|e| format!("Failed to write transcript: {}", e))?;
    fs::rename(&temp_path, path)
        .map_err(|e| format!("Failed to finalize transcript: {}", e))?;

    Ok(())
}

/// Migrate a single transcript from plain text to frontmatter format
fn migrate_transcript(path: &Path) -> Result<bool, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read transcript: {}", e))?;

    // Already has frontmatter - skip
    if content.starts_with("---") {
        return Ok(false);
    }

    // Try to get metadata from recording metadata
    let recording_dir = path
        .parent()
        .ok_or("Invalid transcript path")?;

    let recording_id = recording_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Could not determine recording ID")?;

    let (created_at, duration_sec) = match read_metadata(recording_id) {
        Ok(metadata) => {
            let duration = metadata
                .audio
                .mix
                .as_ref()
                .map(|a| a.duration_sec)
                .unwrap_or(0.0);
            (metadata.created_at, duration)
        }
        Err(_) => {
            // Use current time as fallback
            let now = chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            (now, 0.0)
        }
    };

    let transcript_meta = TranscriptMetadata {
        source: TranscriptSource::Local,
        model: "unknown".to_string(),
        created_at,
        duration_sec,
        language: Some("en".to_string()),
    };

    write_transcript_with_metadata(path, &content, &transcript_meta)?;
    Ok(true)
}

/// Migrate all existing transcripts to frontmatter format
pub fn migrate_all_transcripts() -> Result<usize, String> {
    let data_dir = get_data_dir();

    if !data_dir.exists() {
        return Ok(0);
    }

    let mut migrated_count = 0;

    let entries = fs::read_dir(&data_dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let transcript_path = path.join("transcript.md");
            if transcript_path.exists() {
                match migrate_transcript(&transcript_path) {
                    Ok(true) => migrated_count += 1,
                    Ok(false) => {} // Already migrated
                    Err(e) => {
                        // Log but don't stop migration for other files
                        eprintln!(
                            "Warning: Failed to migrate transcript at {}: {}",
                            transcript_path.display(),
                            e
                        );
                    }
                }
            }
        }
    }

    Ok(migrated_count)
}

/// Check if transcript migration has been completed
pub fn is_migration_needed() -> bool {
    // Check for migration flag in settings JSON
    let settings_path = crate::config::get_settings_path();
    if let Ok(content) = fs::read_to_string(settings_path)
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(migrated) = json.get("transcript_migration_done").and_then(|v| v.as_bool())
    {
        return !migrated;
    }
    // If no flag found, migration is needed (but only if there are recordings)
    true
}

/// Mark transcript migration as completed in settings
pub fn mark_migration_done() -> Result<(), String> {
    let settings_path = crate::config::get_settings_path();

    let mut json = if let Ok(content) = fs::read_to_string(&settings_path) {
        serde_json::from_str::<serde_json::Value>(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    json["transcript_migration_done"] = serde_json::Value::Bool(true);

    let config_dir = crate::config::get_config_dir();
    if !config_dir.exists() {
        fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    }

    let content =
        serde_json::to_string_pretty(&json).map_err(|e| format!("Failed to serialize: {}", e))?;
    fs::write(&settings_path, content)
        .map_err(|e| format!("Failed to write settings: {}", e))?;

    Ok(())
}

/// Run transcript migration if needed (called on app startup)
pub fn run_migration_if_needed() {
    if is_migration_needed() {
        match migrate_all_transcripts() {
            Ok(count) => {
                if count > 0 {
                    eprintln!("Migrated {} transcript(s) to frontmatter format", count);
                }
                let _ = mark_migration_done();
            }
            Err(e) => {
                eprintln!("Warning: Transcript migration failed: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_transcript_metadata_serialization() {
        let metadata = TranscriptMetadata {
            source: TranscriptSource::Local,
            model: "whisper-base".to_string(),
            created_at: "2026-02-03T14:30:00Z".to_string(),
            duration_sec: 180.5,
            language: Some("en".to_string()),
        };

        let yaml = serde_yaml::to_string(&metadata).unwrap();
        assert!(yaml.contains("source: local"));
        assert!(yaml.contains("model: whisper-base"));
        assert!(yaml.contains("duration_sec: 180.5"));
    }

    #[test]
    fn test_transcript_metadata_deserialization() {
        let yaml = "source: openai\nmodel: whisper-1\ncreated_at: \"2026-02-03T14:30:00Z\"\nduration_sec: 60.0\nlanguage: en\n";
        let metadata: TranscriptMetadata = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(metadata.source, TranscriptSource::Openai));
        assert_eq!(metadata.model, "whisper-1");
        assert_eq!(metadata.duration_sec, 60.0);
    }

    #[test]
    fn test_read_transcript_with_frontmatter() {
        let content =
            "---\nsource: local\nmodel: whisper-base\ncreated_at: \"2026-02-03T14:30:00Z\"\nduration_sec: 180.5\n---\n\nThis is the transcript content.";

        let mut temp = NamedTempFile::new().unwrap();
        write!(temp, "{}", content).unwrap();

        let result = read_transcript(temp.path()).unwrap();
        assert!(result.metadata.is_some());
        let meta = result.metadata.unwrap();
        assert!(matches!(meta.source, TranscriptSource::Local));
        assert_eq!(result.content, "This is the transcript content.");
    }

    #[test]
    fn test_read_transcript_legacy_format() {
        let content = "This is a plain transcript without frontmatter.";

        let mut temp = NamedTempFile::new().unwrap();
        write!(temp, "{}", content).unwrap();

        let result = read_transcript(temp.path()).unwrap();
        assert!(result.metadata.is_none());
        assert_eq!(result.content, content);
    }

    #[test]
    fn test_write_transcript_with_metadata() {
        let metadata = TranscriptMetadata {
            source: TranscriptSource::Local,
            model: "whisper-base".to_string(),
            created_at: "2026-02-03T14:30:00Z".to_string(),
            duration_sec: 180.5,
            language: Some("en".to_string()),
        };

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("transcript.md");

        write_transcript_with_metadata(&path, "Test content", &metadata).unwrap();

        // Read it back
        let result = read_transcript(&path).unwrap();
        assert!(result.metadata.is_some());
        assert_eq!(result.content, "Test content");
    }

    #[test]
    fn test_idempotent_migration() {
        let content =
            "---\nsource: local\nmodel: whisper-base\ncreated_at: \"2026-02-03T14:30:00Z\"\nduration_sec: 0.0\n---\n\nAlready migrated content";

        let mut temp = NamedTempFile::new().unwrap();
        write!(temp, "{}", content).unwrap();

        // Migration should be skipped for already-migrated content
        let result = migrate_transcript(temp.path()).unwrap();
        assert!(!result); // false = not migrated (already done)
    }

    #[test]
    fn test_transcript_source_variants() {
        let sources = vec![
            ("local", TranscriptSource::Local),
            ("openai", TranscriptSource::Openai),
            ("google", TranscriptSource::Google),
            ("anthropic", TranscriptSource::Anthropic),
        ];

        for (expected_str, source) in sources {
            let json = serde_json::to_string(&source).unwrap();
            assert_eq!(json, format!("\"{}\"", expected_str));
        }
    }

}
