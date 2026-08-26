use super::protocol::WorkerCommand;
use tokio::sync::mpsc;

pub(super) struct CancelOnDrop {
    tx: mpsc::UnboundedSender<WorkerCommand>,
    id: Option<String>,
}

impl CancelOnDrop {
    pub(super) fn new(tx: mpsc::UnboundedSender<WorkerCommand>, id: String) -> Self {
        Self { tx, id: Some(id) }
    }

    pub(super) fn disarm(mut self) {
        self.id = None;
    }

    pub(super) fn cancel_timed_out(mut self) {
        if let Some(id) = self.id.take() {
            let _ = self.tx.send(WorkerCommand::Cancel {
                id,
                restart_if_queued: true,
            });
        }
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let _ = self.tx.send(WorkerCommand::Cancel {
                id,
                restart_if_queued: false,
            });
        }
    }
}
