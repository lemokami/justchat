//! JSON-RPC id compatibility shim.
//!
//! `agent-client-protocol` 0.4.x assumes JSON-RPC `id`s are integers, but
//! `kiro-cli` (and the JSON-RPC 2.0 spec) use string ids for agent-initiated
//! requests (`session/request_permission`, `fs/*`, `terminal/*`). Without
//! translation those requests fail to parse and are dropped, hanging the agent.
//!
//! This shim sits between the subprocess and the ACP connection and rewrites:
//! - **incoming** agent→client *requests* with a string `id` → a synthesized
//!   integer id (remembering the original), and
//! - **outgoing** client→agent *responses* carrying that integer id → back to
//!   the original string id.
//!
//! Client-initiated requests (which already use integer ids) and notifications
//! pass through untouched, so the shim is transparent to numeric-id agents too.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use futures::io::BufReader;
use futures::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Synthesized ids start high to avoid colliding with the client's own
/// (small, incrementing) request ids.
const ID_BASE: i64 = 1_000_000;

/// Wrap the agent's stdio with the id-translation shim, returning the
/// `(outgoing, incoming)` byte streams to hand to the ACP connection.
///
/// Spawns two pump tasks on the current `LocalSet`.
#[allow(clippy::type_complexity)]
pub fn wrap<W, R>(
    agent_stdin: W,
    agent_stdout: R,
) -> (
    impl AsyncWrite + Unpin + 'static,
    impl AsyncRead + Unpin + 'static,
)
where
    W: AsyncWrite + Unpin + 'static,
    R: AsyncRead + Unpin + 'static,
{
    let map: Rc<RefCell<HashMap<i64, String>>> = Rc::new(RefCell::new(HashMap::new()));
    let counter = Rc::new(RefCell::new(ID_BASE));

    // Duplex pipes connecting the shim to the ACP connection.
    let (shim_to_conn, conn_incoming) = tokio::io::duplex(1 << 16);
    let (conn_outgoing, shim_from_conn) = tokio::io::duplex(1 << 16);

    // Agent -> (rewrite) -> connection.
    {
        let map = map.clone();
        let counter = counter.clone();
        let mut reader = BufReader::new(agent_stdout);
        let mut writer = shim_to_conn.compat_write();
        tokio::task::spawn_local(async move {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let out = rewrite_incoming(&line, &map, &counter);
                        if writer.write_all(out.as_bytes()).await.is_err() {
                            break;
                        }
                        let _ = writer.flush().await;
                    }
                }
            }
        });
    }

    // Connection -> (rewrite) -> agent.
    {
        let map = map.clone();
        let mut reader = BufReader::new(shim_from_conn.compat());
        let mut writer = agent_stdin;
        tokio::task::spawn_local(async move {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let out = rewrite_outgoing(&line, &map);
                        if writer.write_all(out.as_bytes()).await.is_err() {
                            break;
                        }
                        let _ = writer.flush().await;
                    }
                }
            }
        });
    }

    (conn_outgoing.compat_write(), conn_incoming.compat())
}

/// Rewrite an agent→client line: string-id requests get a synthesized integer
/// id (remembered for the response). Everything else passes through.
fn rewrite_incoming(
    line: &str,
    map: &Rc<RefCell<HashMap<i64, String>>>,
    counter: &Rc<RefCell<i64>>,
) -> String {
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(line) else {
        return line.to_string();
    };
    let is_request = v.get("method").is_some();
    let string_id = v.get("id").and_then(|id| id.as_str()).map(str::to_owned);

    if is_request {
        if let Some(orig) = string_id {
            let n = {
                let mut c = counter.borrow_mut();
                let n = *c;
                *c += 1;
                n
            };
            map.borrow_mut().insert(n, orig);
            v["id"] = serde_json::Value::from(n);
            return to_line(&v);
        }
    }
    line.to_string()
}

/// Rewrite a client→agent line: responses whose integer id maps to a remembered
/// string id are translated back. Everything else passes through.
fn rewrite_outgoing(line: &str, map: &Rc<RefCell<HashMap<i64, String>>>) -> String {
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(line) else {
        return line.to_string();
    };
    let is_response = v.get("result").is_some() || v.get("error").is_some();
    let int_id = v.get("id").and_then(|id| id.as_i64());

    if is_response {
        if let Some(n) = int_id {
            if let Some(orig) = map.borrow_mut().remove(&n) {
                v["id"] = serde_json::Value::from(orig);
                return to_line(&v);
            }
        }
    }
    line.to_string()
}

fn to_line(v: &serde_json::Value) -> String {
    let mut s = serde_json::to_string(v).unwrap_or_default();
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    type IdMap = Rc<RefCell<HashMap<i64, String>>>;

    fn new_map() -> (IdMap, Rc<RefCell<i64>>) {
        (
            Rc::new(RefCell::new(HashMap::new())),
            Rc::new(RefCell::new(ID_BASE)),
        )
    }

    #[test]
    fn rewrites_string_id_request_and_maps_response_back() {
        let (map, counter) = new_map();
        let req =
            r#"{"jsonrpc":"2.0","method":"session/request_permission","params":{},"id":"abc-123"}"#;
        let rewritten = rewrite_incoming(req, &map, &counter);
        let v: serde_json::Value = serde_json::from_str(&rewritten).unwrap();
        let n = v["id"].as_i64().unwrap();
        assert_eq!(n, ID_BASE);
        assert_eq!(map.borrow().get(&n).map(String::as_str), Some("abc-123"));

        // The client's response carries the integer id; translate it back.
        let resp = format!(r#"{{"jsonrpc":"2.0","result":{{"ok":true}},"id":{n}}}"#);
        let out = rewrite_outgoing(&resp, &map);
        let ov: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(ov["id"].as_str(), Some("abc-123"));
        assert!(map.borrow().is_empty(), "mapping consumed after response");
    }

    #[test]
    fn passes_through_numeric_id_and_notifications() {
        let (map, counter) = new_map();
        // Agent response to a client request (numeric id) — unchanged.
        let resp = r#"{"jsonrpc":"2.0","result":{},"id":5}"#;
        assert_eq!(rewrite_incoming(resp, &map, &counter), resp);
        // Notification (no id) — unchanged.
        let note = r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#;
        assert_eq!(rewrite_incoming(note, &map, &counter), note);
        assert!(map.borrow().is_empty());
    }

    #[test]
    fn outgoing_client_request_untouched() {
        let map: Rc<RefCell<HashMap<i64, String>>> = Rc::new(RefCell::new(HashMap::new()));
        let req = r#"{"jsonrpc":"2.0","method":"initialize","params":{},"id":0}"#;
        assert_eq!(rewrite_outgoing(req, &map), req);
    }
}
