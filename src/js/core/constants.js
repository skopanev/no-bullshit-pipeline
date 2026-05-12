// Shared frontend constants — keep in sync with src-tauri/src/config.rs.

/// Canonical default Whisper model filename. Used as a fallback wherever the
/// user hasn't picked a specific model. Mirrors `config::DEFAULT_WHISPER_MODEL`.
export const DEFAULT_WHISPER_MODEL = 'ggml-base.bin';
