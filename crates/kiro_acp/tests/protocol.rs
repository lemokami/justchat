//! Integration tests for the ACP protocol layer (Tasks 3 & 4).
//!
//! These spawn the scripted `mock_agent` binary as a subprocess and drive a
//! real [`agent_client_protocol::ClientSideConnection`] through it.

use kiro_acp::protocol::{self, Event};
use kiro_acp::{Subprocess, SubprocessConfig};

/// Spawn the mock agent and run `body` on a LocalSet with a live connection.
async fn with_mock_agent<F, Fut>(env: &[(&str, &str)], body: F)
where
    F: FnOnce(
        std::rc::Rc<agent_client_protocol::ClientSideConnection>,
        kiro_acp::EventReceiver,
    ) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let local = tokio::task::LocalSet::new();
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    local
        .run_until(async move {
            let bin = env!("CARGO_BIN_EXE_mock_agent");
            let cwd = std::env::current_dir().unwrap();
            let mut config = SubprocessConfig::command(bin, Vec::<String>::new(), cwd);
            for (k, v) in &env {
                config = config.with_env(k, v);
            }
            let (proc, outgoing, incoming) = Subprocess::spawn(&config).expect("spawn mock agent");

            let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
            let (conn, _shared, io) = protocol::new_connection(event_tx, outgoing, incoming);
            tokio::task::spawn_local(async move {
                let _ = io.await;
            });

            body(conn, event_rx).await;

            let _ = proc.shutdown().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn handshake_negotiates_v1() {
    with_mock_agent(&[], |conn, _rx| async move {
        let info = protocol::initialize(&conn).await.expect("initialize");
        assert_eq!(info.protocol_version, 1);
        assert!(info.supported, "v1 should be supported");
        assert!(info.load_session, "mock agent advertises loadSession");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn handshake_reports_unsupported_version() {
    with_mock_agent(&[("KIRO_MOCK_BAD_VERSION", "1")], |conn, _rx| async move {
        let info = protocol::initialize(&conn).await.expect("initialize");
        assert_eq!(info.protocol_version, 0);
        assert!(!info.supported, "v0 must be flagged unsupported");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn prompt_round_trip_emits_ordered_events() {
    with_mock_agent(&[], |conn, mut rx| async move {
        protocol::initialize(&conn).await.expect("initialize");
        let session_id = protocol::create_session(&conn, std::env::current_dir().unwrap())
            .await
            .expect("create session")
            .id;
        assert_eq!(session_id, "mock-session-0");

        let stop = protocol::send_prompt(&conn, &session_id, "ping".into(), vec![])
            .await
            .expect("prompt");
        assert_eq!(stop, "end_turn");

        // Drain the events emitted during the turn.
        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        // Expect: thought, then two message chunks (in order).
        assert_eq!(
            events,
            vec![
                Event::ThoughtChunk {
                    session_id: session_id.clone(),
                    text: "thinking about it".into(),
                },
                Event::MessageChunk {
                    session_id: session_id.clone(),
                    text: "Hello! You said: ".into(),
                },
                Event::MessageChunk {
                    session_id: session_id.clone(),
                    text: "ping".into(),
                },
            ]
        );

        // Assembled visible transcript.
        let transcript: String = events
            .iter()
            .filter_map(|e| match e {
                Event::MessageChunk { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(transcript, "Hello! You said: ping");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn prompt_emits_tool_call_events() {
    with_mock_agent(&[("KIRO_MOCK_EMIT_TOOL", "1")], |conn, mut rx| async move {
        protocol::initialize(&conn).await.expect("initialize");
        let session_id = protocol::create_session(&conn, std::env::current_dir().unwrap())
            .await
            .expect("create session")
            .id;
        protocol::send_prompt(&conn, &session_id, "read".into(), vec![])
            .await
            .expect("prompt");

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        let has_tool_call = events.iter().any(|e| {
            matches!(e, Event::ToolCall { title, kind, .. } if title == "Read file" && kind == "read")
        });
        let has_completed = events.iter().any(|e| {
            matches!(e, Event::ToolCallUpdate { status: Some(s), output: Some(o), .. }
                if s == "completed" && o == "file contents")
        });
        assert!(has_tool_call, "expected a ToolCall event: {events:?}");
        assert!(has_completed, "expected a completed ToolCallUpdate: {events:?}");
    })
    .await;
}
