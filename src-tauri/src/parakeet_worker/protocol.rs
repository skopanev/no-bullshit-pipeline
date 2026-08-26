use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tauri_plugin_shell::process::CommandChild;
use tokio::sync::oneshot;

pub(super) enum WorkerCommand {
    Transcribe {
        request: ResidentRequest,
        temp_path: OwnedTempPath,
        dispatched: oneshot::Sender<()>,
        reply: oneshot::Sender<Result<String, String>>,
    },
    Health {
        request: ResidentRequest,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Cancel {
        id: String,
        restart_if_queued: bool,
    },
    Shutdown,
}

pub(super) enum ControlResult {
    Handled,
    Cancel { id: String, restart_if_queued: bool },
    Shutdown,
    Forward(WorkerCommand),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ResidentRequest {
    id: String,
    operation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    wav_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vocab_path: Option<String>,
    translit_threshold: f32,
    translit_min_len: u32,
}

impl ResidentRequest {
    pub(super) fn id(&self) -> &str {
        &self.id
    }

    pub(super) fn ping(id: String) -> Self {
        Self {
            id,
            operation: "ping".into(),
            wav_path: None,
            vocab_path: None,
            translit_threshold: 0.0,
            translit_min_len: 0,
        }
    }

    pub(super) fn transcribe(
        id: String,
        wav_path: String,
        vocab_path: Option<String>,
        translit_threshold: f32,
        translit_min_len: u32,
    ) -> Self {
        Self {
            id,
            operation: "transcribe".into(),
            wav_path: Some(wav_path),
            vocab_path,
            translit_threshold,
            translit_min_len,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(super) enum ResidentEvent {
    Ready,
    Pong {
        id: String,
    },
    Result {
        id: String,
        text: String,
        model: String,
    },
    Error {
        id: Option<String>,
        message: String,
    },
    Fatal {
        message: String,
    },
}

pub(super) enum PendingReply {
    Transcribe {
        _temp_path: OwnedTempPath,
        reply: oneshot::Sender<Result<String, String>>,
    },
    Health {
        reply: oneshot::Sender<Result<(), String>>,
    },
}

pub(super) struct OwnedTempPath(PathBuf);

impl OwnedTempPath {
    pub(super) fn new(path: PathBuf) -> Self {
        Self(path)
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for OwnedTempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(super) fn write_mono_wav(path: &Path, samples: &[f32]) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|error| error.to_string())?;
    for &sample in samples {
        writer
            .write_sample((sample * 32768.0).clamp(-32768.0, 32767.0) as i16)
            .map_err(|error| error.to_string())?;
    }
    writer.finalize().map_err(|error| error.to_string())
}

#[derive(Default)]
pub(super) struct NdjsonBuffer(Vec<u8>);

impl NdjsonBuffer {
    pub(super) fn push(&mut self, chunk: &[u8]) -> Vec<Result<ResidentEvent, String>> {
        self.0.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(index) = self.0.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.0.drain(..=index).collect();
            let line = &line[..line.len().saturating_sub(1)];
            if !line.is_empty() {
                events.push(serde_json::from_slice(line).map_err(|error| error.to_string()));
            }
        }
        events
    }
}

pub(super) fn handle_control(command: WorkerCommand, ready: bool, busy: bool) -> ControlResult {
    match command {
        WorkerCommand::Health { reply, .. } if !ready => {
            let _ = reply.send(Err("resident Parakeet is not ready".into()));
            ControlResult::Handled
        }
        WorkerCommand::Health { reply, .. } if busy => {
            let _ = reply.send(Ok(()));
            ControlResult::Handled
        }
        WorkerCommand::Cancel {
            id,
            restart_if_queued,
        } => ControlResult::Cancel {
            id,
            restart_if_queued,
        },
        WorkerCommand::Shutdown => ControlResult::Shutdown,
        command => ControlResult::Forward(command),
    }
}

pub(super) fn handle_request(
    command: WorkerCommand,
    child: &mut CommandChild,
    pending: &mut HashMap<String, PendingReply>,
) {
    let (request, pending_reply, dispatched) = match command {
        WorkerCommand::Transcribe {
            request,
            temp_path,
            dispatched,
            reply,
        } => (
            request,
            PendingReply::Transcribe {
                _temp_path: temp_path,
                reply,
            },
            Some(dispatched),
        ),
        WorkerCommand::Health { request, reply } => (request, PendingReply::Health { reply }, None),
        _ => return,
    };
    let id = request.id.clone();
    pending.insert(id.clone(), pending_reply);
    match write_request(child, &request) {
        Ok(()) => {
            if let Some(dispatched) = dispatched {
                let _ = dispatched.send(());
            }
        }
        Err(error) => {
            if let Some(reply) = pending.remove(&id) {
                fail_reply(reply, error);
            }
        }
    }
}

pub(super) fn handle_event(
    event: ResidentEvent,
    pending: &mut HashMap<String, PendingReply>,
) -> bool {
    match event {
        ResidentEvent::Result { id, text, model } => {
            let _ = model;
            if let Some(PendingReply::Transcribe { reply, .. }) = pending.remove(&id) {
                let _ = reply.send(Ok(text));
            }
        }
        ResidentEvent::Error {
            id: Some(id),
            message,
        } => {
            if let Some(reply) = pending.remove(&id) {
                fail_reply(reply, message);
            }
        }
        ResidentEvent::Error { id: None, message } => {
            log::warn!("resident Parakeet error: {message}");
        }
        ResidentEvent::Fatal { message } => {
            log::warn!("resident Parakeet fatal: {message}");
            return true;
        }
        ResidentEvent::Pong { id } => {
            if let Some(PendingReply::Health { reply }) = pending.remove(&id) {
                let _ = reply.send(Ok(()));
            }
        }
        ResidentEvent::Ready => {}
    }
    false
}

pub(super) fn fail_all(pending: &mut HashMap<String, PendingReply>, message: &str) {
    for (_, reply) in pending.drain() {
        fail_reply(reply, message.to_string());
    }
}

pub(super) fn cancel_pending(pending: &mut HashMap<String, PendingReply>, id: &str) -> bool {
    if let Some(reply) = pending.remove(id) {
        fail_reply(reply, "resident Parakeet request cancelled".into());
        true
    } else {
        false
    }
}

pub(super) fn reject_command(command: WorkerCommand, message: &str) {
    match command {
        WorkerCommand::Transcribe { reply, .. } => {
            let _ = reply.send(Err(message.into()));
        }
        WorkerCommand::Health { reply, .. } => {
            let _ = reply.send(Err(message.into()));
        }
        _ => {}
    }
}

fn write_request(child: &mut CommandChild, request: &ResidentRequest) -> Result<(), String> {
    let mut line = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    line.push(b'\n');
    child.write(&line).map_err(|error| error.to_string())
}

fn fail_reply(reply: PendingReply, message: String) {
    match reply {
        PendingReply::Transcribe { reply, .. } => {
            let _ = reply.send(Err(message));
        }
        PendingReply::Health { reply } => {
            let _ = reply.send(Err(message));
        }
    }
}
