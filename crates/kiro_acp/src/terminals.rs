//! Client-side terminal management for the ACP `terminal/*` methods.
//!
//! Spawns commands the agent requests, captures their combined output into an
//! in-memory buffer, and tracks exit status. Everything lives on the protocol
//! thread, so interior mutability uses `Rc`/`RefCell` (no locking needed).

use std::cell::RefCell;
use std::process::Stdio;
use std::rc::Rc;

use agent_client_protocol as acp;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// A single running (or finished) terminal.
struct Terminal {
    child: Option<tokio::process::Child>,
    output: Rc<RefCell<String>>,
    exited: Rc<RefCell<Option<acp::TerminalExitStatus>>>,
}

/// Tracks all terminals created during the session.
#[derive(Default)]
pub struct TerminalManager {
    next_id: u64,
    terminals: std::collections::HashMap<String, Terminal>,
}

fn exit_status(status: std::process::ExitStatus) -> acp::TerminalExitStatus {
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|s| s.to_string())
    };
    #[cfg(not(unix))]
    let signal: Option<String> = None;

    acp::TerminalExitStatus {
        exit_code: status.code().map(|c| c as u32),
        signal,
        meta: None,
    }
}

impl TerminalManager {
    /// Spawn a command and begin capturing its output. Returns the new id.
    pub fn create(&mut self, req: &acp::CreateTerminalRequest) -> std::io::Result<acp::TerminalId> {
        let mut command = Command::new(&req.command);
        command
            .args(&req.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &req.cwd {
            command.current_dir(cwd);
        }
        for env in &req.env {
            command.env(&env.name, &env.value);
        }

        let mut child = command.spawn()?;

        let output = Rc::new(RefCell::new(String::new()));
        let exited = Rc::new(RefCell::new(None));

        // Capture stdout and stderr into the shared buffer.
        if let Some(stdout) = child.stdout.take() {
            spawn_reader(stdout, output.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_reader(stderr, output.clone());
        }

        self.next_id += 1;
        let id = format!("term-{}", self.next_id);
        self.terminals.insert(
            id.clone(),
            Terminal {
                child: Some(child),
                output,
                exited,
            },
        );
        Ok(acp::TerminalId(id.into()))
    }

    /// Current output + exit status (non-blocking).
    pub fn output(&mut self, id: &acp::TerminalId) -> Option<acp::TerminalOutputResponse> {
        let term = self.terminals.get_mut(id.0.as_ref())?;
        // Poll for exit without blocking.
        if term.exited.borrow().is_none() {
            if let Some(child) = term.child.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    *term.exited.borrow_mut() = Some(exit_status(status));
                }
            }
        }
        Some(acp::TerminalOutputResponse {
            output: term.output.borrow().clone(),
            truncated: false,
            exit_status: term.exited.borrow().clone(),
            meta: None,
        })
    }

    /// Block until the terminal's command exits, returning its status.
    pub async fn wait_for_exit(&mut self, id: &acp::TerminalId) -> Option<acp::TerminalExitStatus> {
        let term = self.terminals.get_mut(id.0.as_ref())?;
        if let Some(status) = term.exited.borrow().clone() {
            return Some(status);
        }
        let child = term.child.as_mut()?;
        let status = child.wait().await.ok()?;
        let status = exit_status(status);
        *term.exited.borrow_mut() = Some(status.clone());
        Some(status)
    }

    /// Kill the terminal's command without releasing the terminal.
    pub fn kill(&mut self, id: &acp::TerminalId) {
        if let Some(term) = self.terminals.get_mut(id.0.as_ref()) {
            if let Some(child) = term.child.as_mut() {
                let _ = child.start_kill();
            }
        }
    }

    /// Release (remove) a terminal and free its resources.
    pub fn release(&mut self, id: &acp::TerminalId) {
        self.terminals.remove(id.0.as_ref());
    }
}

fn spawn_reader<R>(mut reader: R, buffer: Rc<RefCell<String>>)
where
    R: AsyncReadExt + Unpin + 'static,
{
    tokio::task::spawn_local(async move {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]);
                    buffer.borrow_mut().push_str(&chunk);
                }
            }
        }
    });
}
