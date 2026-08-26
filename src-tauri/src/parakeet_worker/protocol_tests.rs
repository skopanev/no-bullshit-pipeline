use super::cancellation::CancelOnDrop;
use super::protocol::*;
use super::supervisor::{ServeExit, active_cancel_outcome, cancel_queued};
use std::collections::{HashMap, VecDeque};
use tokio::sync::{mpsc, oneshot};

#[test]
fn health_is_forwarded_to_ready_child() {
    let (reply, mut receiver) = oneshot::channel();
    let command = WorkerCommand::Health {
        request: ResidentRequest::ping("health".into()),
        reply,
    };
    assert!(matches!(
        handle_control(command, true, false),
        ControlResult::Forward(WorkerCommand::Health { .. })
    ));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn health_is_rejected_without_ready_child() {
    let (reply, mut receiver) = oneshot::channel();
    let command = WorkerCommand::Health {
        request: ResidentRequest::ping("health".into()),
        reply,
    };
    assert!(matches!(
        handle_control(command, false, false),
        ControlResult::Handled
    ));
    assert!(matches!(receiver.try_recv(), Ok(Err(_))));
}

#[test]
fn health_is_accepted_locally_while_transcription_is_busy() {
    let (reply, mut receiver) = oneshot::channel();
    let command = WorkerCommand::Health {
        request: ResidentRequest::ping("health".into()),
        reply,
    };
    assert!(matches!(
        handle_control(command, true, true),
        ControlResult::Handled
    ));
    assert!(matches!(receiver.try_recv(), Ok(Ok(()))));
}

#[test]
fn pong_completes_matching_health_request() {
    let (reply, mut receiver) = oneshot::channel();
    let mut pending = HashMap::from([("health".into(), PendingReply::Health { reply })]);
    assert!(!handle_event(
        ResidentEvent::Pong {
            id: "health".into()
        },
        &mut pending
    ));
    assert!(matches!(receiver.try_recv(), Ok(Ok(()))));
    assert!(pending.is_empty());
}

#[test]
fn dropping_request_guard_sends_cancel() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    drop(CancelOnDrop::new(tx, "request".into()));
    assert!(matches!(
        rx.try_recv(),
        Ok(WorkerCommand::Cancel {
            id,
            restart_if_queued: false
        }) if id == "request"
    ));
}

#[test]
fn timeout_guard_requests_queued_restart() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    CancelOnDrop::new(tx, "request".into()).cancel_timed_out();
    assert!(matches!(
        rx.try_recv(),
        Ok(WorkerCommand::Cancel {
            id,
            restart_if_queued: true
        }) if id == "request"
    ));
}

#[test]
fn disarmed_request_guard_does_not_cancel() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    CancelOnDrop::new(tx, "request".into()).disarm();
    assert!(rx.try_recv().is_err());
}

#[test]
fn rejected_transcribe_closes_dispatch_ack_before_reply() {
    let (dispatched, mut dispatch_receiver) = oneshot::channel();
    let (reply, mut receiver) = oneshot::channel();
    let command = WorkerCommand::Transcribe {
        request: ResidentRequest::transcribe(
            "request".into(),
            "/tmp/nbp-dispatch-test.wav".into(),
            None,
            0.0,
            0,
        ),
        temp_path: OwnedTempPath::new("/tmp/nbp-dispatch-test.wav".into()),
        dispatched,
        reply,
    };
    reject_command(command, "not dispatched");
    assert!(matches!(
        dispatch_receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Closed)
    ));
    assert!(matches!(receiver.try_recv(), Ok(Err(error)) if error == "not dispatched"));
}

#[test]
fn cancel_removes_active_request() {
    let (reply, mut receiver) = oneshot::channel();
    let mut pending = HashMap::from([("health".into(), PendingReply::Health { reply })]);
    assert!(cancel_pending(&mut pending, "health"));
    assert!(matches!(receiver.try_recv(), Ok(Err(_))));
    assert!(pending.is_empty());
}

#[test]
fn active_cancel_restarts_even_without_queued_restart_flag() {
    let (reply, mut receiver) = oneshot::channel();
    let temp_path = OwnedTempPath::new(std::env::temp_dir().join("nbp-cancel-test.wav"));
    let mut pending = HashMap::from([(
        "request".into(),
        PendingReply::Transcribe {
            _temp_path: temp_path,
            reply,
        },
    )]);
    assert!(matches!(
        active_cancel_outcome(&mut pending, "request"),
        Some(ServeExit::RestartNow)
    ));
    assert!(matches!(receiver.try_recv(), Ok(Err(_))));
}

#[test]
fn cancel_removes_request_queued_during_startup() {
    let (reply, mut receiver) = oneshot::channel();
    let mut queued = VecDeque::from([WorkerCommand::Health {
        request: ResidentRequest::ping("health".into()),
        reply,
    }]);
    assert!(cancel_queued(&mut queued, "health"));
    assert!(matches!(receiver.try_recv(), Ok(Err(_))));
    assert!(queued.is_empty());
}

#[test]
fn ndjson_buffer_preserves_fragmented_event() {
    let mut buffer = NdjsonBuffer::default();
    assert!(buffer.push(b"{\"type\":\"res").is_empty());
    let events = buffer.push(b"ult\",\"id\":\"a\",\"text\":\"hello\",\"model\":\"parakeet\"}\n");
    assert_eq!(events.len(), 1);
    assert!(matches!(events.into_iter().next().unwrap().unwrap(),
        ResidentEvent::Result { id, text, .. } if id == "a" && text == "hello"));
}

#[test]
fn ndjson_buffer_returns_multiple_events_from_one_chunk() {
    let mut buffer = NdjsonBuffer::default();
    let events = buffer.push(b"{\"type\":\"ready\"}\n{\"type\":\"pong\",\"id\":\"b\"}\n");
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], Ok(ResidentEvent::Ready)));
    assert!(matches!(&events[1], Ok(ResidentEvent::Pong { id }) if id == "b"));
}
