//! End-to-end test of the channel bridge (Task 5) against the mock agent.
//!
//! Drives the full prompt round-trip purely through the [`Command`]/[`Event`]
//! channel API, with no direct ACP calls.

use std::time::Duration;

use kiro_acp::bridge::{self, BridgeConfig};
use kiro_acp::protocol::{Command, Event};
use kiro_acp::SubprocessConfig;

async fn recv(rx: &mut kiro_acp::protocol::EventReceiver) -> Event {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("timed out waiting for event")
        .expect("event channel closed")
}

fn mock_config() -> BridgeConfig {
    let bin = env!("CARGO_BIN_EXE_mock_agent");
    let cwd = std::env::current_dir().unwrap();
    BridgeConfig {
        subprocess: SubprocessConfig::command(bin, Vec::<String>::new(), cwd),
        settings: Default::default(),
    }
}

#[tokio::test]
async fn bridge_full_round_trip() {
    let mut handle = bridge::start(mock_config()).expect("start bridge");
    let mut events = handle.take_events().expect("events");

    // Handshake.
    match recv(&mut events).await {
        Event::Connected {
            protocol_version,
            supported,
            ..
        } => {
            assert_eq!(protocol_version, 1);
            assert!(supported);
        }
        other => panic!("expected Connected, got {other:?}"),
    }

    // Create a session.
    handle.send(Command::CreateSession);
    let session_id = match recv(&mut events).await {
        Event::SessionCreated { session_id } => session_id,
        other => panic!("expected SessionCreated, got {other:?}"),
    };
    assert_eq!(session_id, "mock-session-0");

    // Send a prompt and collect events until the turn ends.
    handle.send(Command::SendPrompt {
        session_id: session_id.clone(),
        text: "hi".into(),
        attachments: vec![],
    });

    let mut chunks = String::new();
    let mut saw_thought = false;
    let stop_reason;
    // Collect turn events. Chunk notifications and the turn-end response are
    // dispatched independently by the ACP runtime, so after TurnEnded we drain
    // any immediately-available residual chunk events before asserting.
    loop {
        match recv(&mut events).await {
            Event::ThoughtChunk { .. } => saw_thought = true,
            Event::MessageChunk { text, .. } => chunks.push_str(&text),
            Event::TurnEnded {
                stop_reason: sr, ..
            } => {
                stop_reason = sr;
                break;
            }
            other => panic!("unexpected event during turn: {other:?}"),
        }
    }
    while let Ok(ev) = events.try_recv() {
        if let Event::MessageChunk { text, .. } = ev {
            chunks.push_str(&text);
        }
    }

    assert!(saw_thought, "should have seen a thought chunk");
    assert_eq!(chunks, "Hello! You said: hi");
    assert_eq!(stop_reason, "end_turn");

    // Clean shutdown joins the worker thread and reaps the subprocess.
    handle.shutdown_and_join();
}

#[tokio::test]
async fn bridge_reports_missing_agent() {
    let cwd = std::env::current_dir().unwrap();
    let config = BridgeConfig {
        subprocess: SubprocessConfig::command(
            "definitely-not-real-agent-xyz",
            Vec::<String>::new(),
            cwd,
        ),
        settings: Default::default(),
    };
    let mut handle = bridge::start(config).expect("start bridge");
    let mut events = handle.take_events().expect("events");
    match recv(&mut events).await {
        Event::Disconnected { message } => {
            assert!(message.contains("failed to start agent"), "got: {message}");
        }
        other => panic!("expected Disconnected, got {other:?}"),
    }
    handle.shutdown_and_join();
}

#[tokio::test]
async fn bridge_permission_round_trip() {
    let bin = env!("CARGO_BIN_EXE_mock_agent");
    let cwd = std::env::current_dir().unwrap();
    let config = BridgeConfig {
        subprocess: SubprocessConfig::command(bin, Vec::<String>::new(), cwd)
            .with_env("KIRO_MOCK_REQUEST_PERMISSION", "1"),
        settings: Default::default(),
    };
    let mut handle = bridge::start(config).expect("start bridge");
    let mut events = handle.take_events().expect("events");

    assert!(matches!(recv(&mut events).await, Event::Connected { .. }));
    handle.send(Command::CreateSession);
    let session_id = match recv(&mut events).await {
        Event::SessionCreated { session_id } => session_id,
        other => panic!("expected SessionCreated, got {other:?}"),
    };

    handle.send(Command::SendPrompt {
        session_id,
        text: "do it".into(),
        attachments: vec![],
    });

    // Collect events until the permission request, then approve it.
    let request_id = loop {
        match recv(&mut events).await {
            Event::PermissionRequested {
                request_id,
                options,
                ..
            } => {
                assert!(options.iter().any(|o| o.id == "allow"));
                break request_id;
            }
            Event::ThoughtChunk { .. }
            | Event::MessageChunk { .. }
            | Event::ToolCall { .. }
            | Event::ToolCallUpdate { .. } => {}
            other => panic!("unexpected before permission: {other:?}"),
        }
    };

    handle.send(Command::PermissionDecision {
        request_id,
        option_id: Some("allow".into()),
    });

    // After approval the turn continues and ends; the mock appends a verdict.
    let mut tail = String::new();
    let stop_reason;
    loop {
        match recv(&mut events).await {
            Event::MessageChunk { text, .. } => tail.push_str(&text),
            Event::TurnEnded {
                stop_reason: sr, ..
            } => {
                stop_reason = sr;
                break;
            }
            Event::ToolCall { .. } | Event::ToolCallUpdate { .. } | Event::ThoughtChunk { .. } => {}
            other => panic!("unexpected after approval: {other:?}"),
        }
    }
    while let Ok(ev) = events.try_recv() {
        if let Event::MessageChunk { text, .. } = ev {
            tail.push_str(&text);
        }
    }

    assert_eq!(stop_reason, "end_turn");
    assert!(
        tail.contains("[permission granted]"),
        "agent should observe approval, got: {tail:?}"
    );

    handle.shutdown_and_join();
}

#[tokio::test]
async fn bridge_detects_agent_crash_mid_turn() {
    let bin = env!("CARGO_BIN_EXE_mock_agent");
    let cwd = std::env::current_dir().unwrap();
    let config = BridgeConfig {
        subprocess: SubprocessConfig::command(bin, Vec::<String>::new(), cwd)
            .with_env("KIRO_MOCK_CRASH_ON_PROMPT", "1"),
        settings: Default::default(),
    };
    let mut handle = bridge::start(config).expect("start bridge");
    let mut events = handle.take_events().expect("events");

    assert!(matches!(recv(&mut events).await, Event::Connected { .. }));
    handle.send(Command::CreateSession);
    let session_id = match recv(&mut events).await {
        Event::SessionCreated { session_id } => session_id,
        other => panic!("expected SessionCreated, got {other:?}"),
    };

    handle.send(Command::SendPrompt {
        session_id,
        text: "boom".into(),
        attachments: vec![],
    });

    // The agent exits mid-turn; the bridge must surface a Disconnected (it may
    // also emit a prompt Error first).
    let mut saw_disconnect = false;
    for _ in 0..5 {
        match recv(&mut events).await {
            Event::Disconnected { .. } => {
                saw_disconnect = true;
                break;
            }
            Event::Error { .. } | Event::ThoughtChunk { .. } | Event::MessageChunk { .. } => {}
            other => panic!("unexpected event after crash: {other:?}"),
        }
    }
    assert!(saw_disconnect, "crash should surface a Disconnected event");

    handle.shutdown_and_join();
}
