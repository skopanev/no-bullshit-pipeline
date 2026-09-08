# No Bullshit Pipeline (NBP)

Total Audio Capture → Structured Data. **Privacy-first. No bullshit.**

## What Is This?

NBP is a high-performance, local-first tool that captures **everything** happening on your Mac's audio system — from your own voice to system sounds, Zoom calls, FaceTime meetings, and Slack huddles. It transforms raw audio into clean, structured data — transcripts and pipeline outputs — without ever sending your sensitive information to the cloud (unless you explicitly route a step through a cloud CLI or API).

**Core Principles:**

- 🔒 **Privacy First**: Everything runs on your Mac. Zero network calls by default.
- 🎙️ **Absolute Capture**: High-quality recording of both Microphone and System Audio (calls, videos, notifications).
- 📁 **Local Ownership**: All data reside in `~/nbp-data/` in universal formats (OGG, JSON, Markdown).
- 🔄 **Derivable Content**: Raw audio is immutable. All transcripts and pipeline outputs are derived artifacts.
- 🎚️ **Professional Signal**: Broadcast-grade normalization (EBU R128) for consistent levels across all sources.

**What It Does:**

- **Capture**: Record Mic + System Audio simultaneously (Meetings, Podcasts, Brainstorms).
- **Auto-record**: Detect calls (Zoom, FaceTime, etc.) and start recording automatically.
- **Process**: Auto-mix and normalize tracks to professional standards.
- **Transcribe**: On-device transcription via FluidAudio, Qwen3, or Apple Speech — fully local, your choice.
- **Automate**: Chain steps into pipelines — feed the transcript into a CLI coding agent, run a shell script, save to disk.
- **Dictate**: Global-hotkey speech-to-text that types into any focused app (Quick Dictate).
- **Organize**: Tag-based filtering, projects, calendar matching, and instant access to raw assets.

## Quick Start

### Prerequisites

- **macOS** (13.0+)
- **Rust** (latest stable)
- **Bun** (fast package manager)

### Build and Run

```bash
# Install dependencies
bun install

# Run development mode (builds sidecars + JS, then launches the app)
bun run dev
```

The app opens automatically. Recordings are saved to `~/nbp-data/`.

> **Releases:** Signed, notarized `.dmg` builds are published to GitHub Releases. `bun run build:release` produces one, but it is **maintainer-only** — it signs, notarizes, and uploads using Apple Developer credentials from a gitignored `.env.release`.

## Features

### Audio

- **Total Audio Loopback**: System audio capture via Core Audio Process Taps (Zoom, FaceTime, browser, etc.) alongside your microphone via cpal.
- **Auto-record Calls**: Detect meeting apps and start recording on their own, tagging the source app (Zoom, FaceTime, …).
- **Real-time Mixing**: Shared ring buffers with adaptive gain control and soft clipping.
- **EBU R128 Normalization**: Per-track loudness normalization (-23 LUFS, -1 dBTP true-peak limiter).
- **OGG Vorbis Encoding**: VBR quality ~128kbps, 48kHz stereo via vorbis_rs.
- **In-app Playback**: Listen to recordings directly in the app.
- **Waveform Visualization**: Real-time visual feedback during recording.
- **Mic Selection**: Choose from available input devices.
- **Auto-discard**: Skip recordings shorter than a configurable threshold (default 3s).
- **Mix-only Mode**: Save only the combined mix by default, keeping storage lean.

### Transcription

- **FluidAudio** (default): Device-local transcription (Parakeet v3) via sidecar binary. No cloud, supports streaming.
- **Qwen3**: Alternative on-device model (`f32` quality or `int8` lighter/faster), batch mode.
- **Apple Speech**: Native macOS `SpeechTranscriber` (macOS 26+), supports streaming.
- **Code-switch Correction**: Optional phonetic recovery of Cyrillic-mangled terms against your word list (Russian).
- **Export**: Transcripts written as source-of-truth JSON plus rendered Markdown.

### Speaker Diarization (who said what)

A native Swift port of the [senko](https://github.com/narcotic-sh/senko) diarization pipeline, running fully on-device — no Python, no cloud:

- **Pipeline**: pyannote segmentation (VAD) → dense subsegments → Kaldi-style fbank (vendored C++) → CAM++ speaker embeddings (CoreML, 192-d) → spectral clustering with automatic speaker-count via the eigenvalue gap. Zero manual thresholds.
- **Source-split mode**: recordings that keep both stems are diarized per channel — remote voices from the clean system tap, "You" from the mic by channel identity (echo is filtered by centroid matching + a time guard). Parallel speech survives with both texts intact, and You/them attribution is exact where a mixed track would be guesswork.
- **One ASR pass**: with Speaker labels enabled, a single Parakeet run produces both the transcript and the speaker map. Mix-only recordings and the manual **Diarize** button use the same engine.
- **Speakers are voiceprints**: each speaker carries a stable uid + CAM++ centroid. Renaming "Speaker 2" to a real name survives re-diarization — voices are re-matched by cosine similarity, not by index.
- **Models bundled**: CAM++ (~14 MB) and pyannote (~5.5 MB) ship inside the app — diarization works offline out of the box.

Credits: [senko](https://github.com/narcotic-sh/senko) (MIT) for the pipeline design, the Kaldi fbank/VAD code and the CAM++ CoreML conversion; [3D-Speaker](https://github.com/modelscope/3D-Speaker) (Apache-2.0) for the CAM++ model and the spectral clustering algorithm; [pyannote](https://github.com/pyannote/pyannote-audio) (MIT) segmentation via [FluidAudio](https://github.com/FluidInference/FluidAudio)'s CoreML conversion.

### Pipelines

- **Multi-step Automation**: Define named pipelines with sequential steps. Visual tile-based builder.
- **Multi-Pipeline**: Assign multiple pipelines to a single recording, each tracked independently.
- **Step Types**:
  - **CLI Agent**: Pipe the transcript through an external coding agent — Claude Code, Codex, OpenCode, or Antigravity — for summaries, notes, or any prompt-driven transform.
  - **Shell**: Run an arbitrary bash script with the step content and template placeholders in the environment (glue your own delivery: `curl`, `cp`, Slack/Notion/Webhook, …).
  - **Save to Folder**: Write the step output to a local folder, with date/time and recording placeholders in the path.
- **Template Placeholders**: Substitute `{transcript}`, `{app}`, `{calendar_title}`, `{calendar_attendees}`, and more into prompts, scripts, and paths.
- **Progress Tracking**: Per-step status (Waiting/Running/Done/Partial) with real-time progress events.
- **Step Output**: Each step's result stored as Markdown under the recording's `pipelines/` folder.

### Quick Dictate

- **Global Hotkey Speech-to-Text**: Press a shortcut, speak, and have the text typed into whatever app is focused.
- **Multiple Shortcuts**: Configure independent shortcuts, each with its own hotkey, engine, and optional pipeline.
- **Trigger Modes**: Toggle (press to start/stop) or Push-to-Talk (hold to record).
- **Input Sources**: Microphone (with optional system loopback) or clipboard text.
- **Ephemeral by Default**: No storage unless you opt to save dictations to the recording list.

### Headless Transcription (`nbp transcribe`)

Transcribe arbitrary audio files from the command line — no window, no recording entry — with the same `nbp` binary that runs the app. Built for the wearable-recorder dump flow: a daemon drops day-long recordings in a folder, `nbp transcribe` turns each into a timecoded `.txt` for downstream LLM processing.

- **Headless**: Drives the FluidAudio (Parakeet v3) sidecar directly; runs on-device, exits when done. Chews ~5h of audio in ~2 min, no chunking.
- **Any format the sidecar decodes**: Audio is read via AVFoundation, so WAV, m4a, mov/qta, … all work — not just WAV. Resampling to 16 kHz is automatic.
- **Wall-clock Timecodes**: Reads the recorder's `YYYYMMDDHHMMSS.WAV` filename as the start time, so every speech segment is stamped with the real time of day it was spoken (else offset from zero).
- **Pause Segmentation**: Splits the transcript at silence gaps (`--pause`, default 3.5s) — one timestamp per spoken block, silence dropped. No diarization.
- **Batch**: Pass multiple files; each gets its own `<file>.txt`.

```bash
# One file → <file>.txt next to it
nbp transcribe ~/voice/20260617125745.WAV

# Custom output path / pause threshold
nbp transcribe day.WAV --out ~/day.txt --pause 3.0

# Batch — each file → its own .txt
nbp transcribe ~/voice/*.WAV
```

Any first argument other than `transcribe` launches the app as usual. The sidecar is resolved automatically (app bundle, dev build, or `--sidecar <path>` / `NBP_SIDECAR`). Output is plain text: a small header plus `[HH:MM:SS] text` lines.

### Calendar

- **Event Matching**: When a recording finalizes, NBP matches it to a nearby Calendar event (Google / iCloud / Exchange — whatever Calendar.app syncs, no extra OAuth).
- **Auto-title & Attendees**: Adopts the event title (when the recording still has a default name) and stamps attendees onto metadata.
- **Pipeline Fuel**: Exposes `{calendar_title}` and `{calendar_attendees}` placeholders for "who did I meet with" automations.
- **Silent & Opt-in**: Runs only when calendar access is granted; never prompts on its own.

### UI

- **Adaptive Themes**: Auto (follows macOS appearance), Light, and Dark.
- **Dynamic Header**: Context-aware UI that transforms based on your current task.
- **Confirmation Modals**: Safety prompts for destructive actions.
- **Finder Integration**: One click to reveal raw files.

## Storage Structure

Recordings are stored in `~/nbp-data/` (configurable), config in `~/.nbp/`:

```
~/nbp-data/
├── {uuid}/
│   ├── metadata.json                  # Title, timestamps, attendees, pipeline run history
│   ├── audio_mix.ogg                  # Combined master mix (always present)
│   ├── raw_mic.ogg                    # Mic track (only if mix-only mode is off)
│   ├── raw_system.ogg                 # System track (only if mix-only mode is off)
│   ├── transcript.json                # Source-of-truth transcript
│   ├── transcript.md                  # Rendered Markdown transcript
│   └── pipelines/
│       └── {pipeline_name}/
│           └── {step_name}.md         # Step output artifact

~/.nbp/
├── settings.json                      # App settings
├── pipelines.json                     # Pipeline definitions
├── asr-models.json                    # Downloaded ASR model tracking
└── models/
    ├── *                              # Downloaded ASR models
    └── llm/                           # Local LLM models (if used)
```

## Privacy & Security

- **Air-Gapped Ready**: Recording and on-device transcription work without internet.
- **No Telemetry**: We don't track what you record, how long you record, or who you are.
- **Secure Storage**: Settings file is `chmod 600`; secrets stay on-device.
- **Encryption**: Bring your own encryption or let macOS handle it via FileVault.
- **Your Choice**: Transcription is 100% on-device; only pipeline steps you explicitly route through a cloud CLI or API leave the machine.

## Installation

Releases are signed with a **Developer ID** and **notarized by Apple**, so they pass Gatekeeper out of the box — just open the `.dmg` and drag `nbp.app` to Applications. No `xattr` workaround needed.

## License

Private project - all rights reserved.

**USE AS-IS. NO WARRANTY.**
