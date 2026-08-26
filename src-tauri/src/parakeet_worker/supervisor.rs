use super::protocol::{
    ControlResult, NdjsonBuffer, PendingReply, ResidentEvent, WorkerCommand, cancel_pending,
    fail_all, handle_control, handle_event, handle_request, reject_command,
};
use std::collections::{HashMap, VecDeque};
use std::time::Duration;
use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tokio::sync::mpsc;

struct SidecarGuard(Option<CommandChild>);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ServeExit {
    Shutdown,
    Restart,
    RestartNow,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StartupExit {
    Ready,
    Shutdown,
    Restart,
    RestartNow,
}

impl Drop for SidecarGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.take() {
            let _ = child.kill();
        }
    }
}

pub(super) async fn supervise(
    app: AppHandle,
    mut commands: mpsc::UnboundedReceiver<WorkerCommand>,
) {
    loop {
        let spawned = app
            .shell()
            .sidecar("fluidaudio-sidecar")
            .and_then(|command| command.args(["--resident-parakeet"]).spawn());
        let (mut events, child) = match spawned {
            Ok(spawned) => spawned,
            Err(error) => {
                log::warn!("resident Parakeet spawn failed: {error}");
                if wait_before_restart(&mut commands).await {
                    return;
                }
                continue;
            }
        };
        let mut child = SidecarGuard(Some(child));
        let mut queued = VecDeque::new();
        let mut buffer = NdjsonBuffer::default();
        let startup = wait_until_ready(&mut events, &mut commands, &mut queued, &mut buffer).await;
        if startup == StartupExit::Shutdown {
            return;
        }
        if startup == StartupExit::RestartNow {
            drop(child);
            continue;
        }
        if startup == StartupExit::Ready {
            // Every fresh child needs one real inference, not just model load.
            // This also closes settings/wake/cancel races where an earlier
            // warmup was attached to the child that just stopped.
            super::warmup::request(&app, "worker_ready");
            let mut pending = HashMap::new();
            let exit = serve(
                &mut events,
                &mut commands,
                &mut queued,
                &mut buffer,
                child.0.as_mut().expect("resident child exists"),
                &mut pending,
            )
            .await;
            fail_all(&mut pending, "resident Parakeet stopped");
            if exit == ServeExit::Shutdown {
                return;
            }
            drop(child);
            if exit == ServeExit::RestartNow {
                continue;
            }
            if wait_before_restart(&mut commands).await {
                return;
            }
            continue;
        }
        reject_queued(&mut queued, "resident Parakeet stopped");
        drop(child);
        if wait_before_restart(&mut commands).await {
            return;
        }
    }
}

async fn wait_until_ready(
    events: &mut mpsc::Receiver<CommandEvent>,
    commands: &mut mpsc::UnboundedReceiver<WorkerCommand>,
    queued: &mut VecDeque<WorkerCommand>,
    buffer: &mut NdjsonBuffer,
) -> StartupExit {
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(CommandEvent::Stdout(chunk)) => for event in buffer.push(&chunk) {
                    match event {
                        Ok(ResidentEvent::Ready) => return StartupExit::Ready,
                        Ok(ResidentEvent::Fatal { message }) => {
                            log::warn!("resident Parakeet fatal: {message}");
                            return StartupExit::Restart;
                        }
                        Err(error) => log::warn!("resident Parakeet invalid event: {error}"),
                        _ => {}
                    }
                },
                Some(CommandEvent::Stderr(chunk)) => log_stderr(&chunk),
                Some(CommandEvent::Error(error)) => {
                    log::warn!("resident Parakeet process error: {error}");
                    return StartupExit::Restart;
                }
                Some(CommandEvent::Terminated(_)) | None => return StartupExit::Restart,
                _ => {}
            },
            command = commands.recv() => match command {
                Some(command) => match handle_control(command, false, false) {
                    ControlResult::Handled => {}
                    ControlResult::Cancel {
                        id,
                        restart_if_queued,
                    } => {
                        if cancel_queued(queued, &id) && restart_if_queued {
                            return StartupExit::RestartNow;
                        }
                    }
                    ControlResult::Shutdown => return StartupExit::Shutdown,
                    ControlResult::Forward(command) => queued.push_back(command),
                },
                None => return StartupExit::Shutdown,
            }
        }
    }
}

async fn serve(
    events: &mut mpsc::Receiver<CommandEvent>,
    commands: &mut mpsc::UnboundedReceiver<WorkerCommand>,
    queued: &mut VecDeque<WorkerCommand>,
    buffer: &mut NdjsonBuffer,
    child: &mut CommandChild,
    pending: &mut HashMap<String, PendingReply>,
) -> ServeExit {
    loop {
        if let Some(command) = queued.pop_front() {
            if let Some(exit) = dispatch(command, child, pending) {
                return exit;
            }
            continue;
        }
        tokio::select! {
            biased;
            event = events.recv() => match event {
                Some(CommandEvent::Stdout(chunk)) => for event in buffer.push(&chunk) {
                    match event {
                        Ok(event) => if handle_event(event, pending) { return ServeExit::Restart; },
                        Err(error) => log::warn!("resident Parakeet invalid event: {error}"),
                    }
                },
                Some(CommandEvent::Stderr(chunk)) => log_stderr(&chunk),
                Some(CommandEvent::Error(error)) => {
                    log::warn!("resident Parakeet process error: {error}");
                    return ServeExit::Restart;
                }
                Some(CommandEvent::Terminated(_)) | None => return ServeExit::Restart,
                _ => {}
            },
            command = commands.recv() => match command {
                Some(command) => if let Some(exit) = dispatch(command, child, pending) { return exit; },
                None => return ServeExit::Shutdown,
            }
        }
    }
}

fn dispatch(
    command: WorkerCommand,
    child: &mut CommandChild,
    pending: &mut HashMap<String, PendingReply>,
) -> Option<ServeExit> {
    let busy = pending
        .values()
        .any(|reply| matches!(reply, PendingReply::Transcribe { .. }));
    match handle_control(command, true, busy) {
        ControlResult::Handled => None,
        // Swift cannot cancel an in-flight transcribe. Once dispatched, every
        // matched cancel must kill and replace the child. `restart_if_queued`
        // matters only before dispatch, while the model is still loading.
        ControlResult::Cancel { id, .. } => active_cancel_outcome(pending, &id),
        ControlResult::Shutdown => Some(ServeExit::Shutdown),
        ControlResult::Forward(command) => {
            handle_request(command, child, pending);
            None
        }
    }
}

pub(super) fn active_cancel_outcome(
    pending: &mut HashMap<String, PendingReply>,
    id: &str,
) -> Option<ServeExit> {
    cancel_pending(pending, id).then_some(ServeExit::RestartNow)
}

async fn wait_before_restart(commands: &mut mpsc::UnboundedReceiver<WorkerCommand>) -> bool {
    let delay = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(delay);
    loop {
        tokio::select! {
            _ = &mut delay => return false,
            command = commands.recv() => match command {
                Some(command) => match handle_control(command, false, false) {
                    ControlResult::Handled => {}
                    ControlResult::Cancel { .. } => {}
                    ControlResult::Shutdown => return true,
                    ControlResult::Forward(command) => {
                        reject_command(command, "resident Parakeet is restarting");
                    }
                },
                None => return true,
            }
        }
    }
}

fn reject_queued(queued: &mut VecDeque<WorkerCommand>, message: &str) {
    while let Some(command) = queued.pop_front() {
        reject_command(command, message);
    }
}

pub(super) fn cancel_queued(queued: &mut VecDeque<WorkerCommand>, id: &str) -> bool {
    let Some(index) = queued.iter().position(|command| match command {
        WorkerCommand::Transcribe { request, .. } | WorkerCommand::Health { request, .. } => {
            request.id() == id
        }
        _ => false,
    }) else {
        return false;
    };
    queued
        .remove(index)
        .map(|command| reject_command(command, "resident Parakeet request cancelled"))
        .is_some()
}

fn log_stderr(chunk: &[u8]) {
    let message = String::from_utf8_lossy(chunk);
    if message.contains("TIMING:") {
        log::info!("resident Parakeet: {}", message.trim());
    } else {
        log::debug!("resident Parakeet: {message}");
    }
}
