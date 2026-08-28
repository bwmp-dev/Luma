//! Command-specific interactive SSH sessions requested through the webview.
//!
//! The frontend owns terminal creation, while the MCP request owns the command.
//! A random request id joins those two halves without ever sending the command
//! through React. The SSH backend uses the command as the channel's exec request,
//! so it reaches Unix and Windows hosts unchanged and completion comes from the
//! SSH exit status rather than shell syntax injected after the command.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;

use crate::mcp::taps::strip_terminal_output;
use crate::mcp::MAX_COMMAND_OUTPUT_BYTES;
use crate::ssh::{SshExit, SSH_AUTHENTICATED_MARKER};

pub(crate) const SESSION_EVENT: &str = "mcp-session-request";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionRequest {
    pub id: String,
    pub grant_id: String,
    pub grant_label: String,
    pub host_id: String,
    pub host_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionOutcome {
    Completed {
        session_id: String,
        exit: SshExit,
        stdout: String,
        truncated: bool,
    },
    Failed(String),
    Unavailable,
    TimedOut(Option<String>),
}

struct CapturedOutput {
    bytes: VecDeque<u8>,
    truncated: bool,
}

impl CapturedOutput {
    fn new() -> Self {
        Self {
            bytes: VecDeque::with_capacity(MAX_COMMAND_OUTPUT_BYTES),
            truncated: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        for byte in bytes {
            if self.bytes.len() == MAX_COMMAND_OUTPUT_BYTES {
                self.bytes.pop_front();
                self.truncated = true;
            }
            self.bytes.push_back(*byte);
        }
    }

    fn snapshot(&self) -> (String, bool) {
        let bytes: Vec<u8> = self.bytes.iter().copied().collect();
        (strip_terminal_output(&bytes), self.truncated)
    }
}

#[derive(Default)]
struct Progress {
    session_id: Option<String>,
    exit: Option<SshExit>,
}

struct PendingSession {
    grant_id: String,
    host_id: String,
    command: Mutex<Option<String>>,
    authenticated: AtomicBool,
    output: Mutex<CapturedOutput>,
    progress: Mutex<Progress>,
    completion: Mutex<Option<oneshot::Sender<SessionOutcome>>>,
}

impl PendingSession {
    fn new(
        grant_id: &str,
        host_id: &str,
        command: &str,
        completion: oneshot::Sender<SessionOutcome>,
    ) -> Self {
        Self {
            grant_id: grant_id.to_string(),
            host_id: host_id.to_string(),
            command: Mutex::new(Some(command.to_string())),
            authenticated: AtomicBool::new(false),
            output: Mutex::new(CapturedOutput::new()),
            progress: Mutex::new(Progress::default()),
            completion: Mutex::new(Some(completion)),
        }
    }

    fn resolve_if_complete(&self) {
        let completed = {
            let progress = self.progress.lock().unwrap();
            match (&progress.session_id, &progress.exit) {
                (Some(session_id), Some(exit)) => Some((session_id.clone(), exit.clone())),
                _ => None,
            }
        };
        let Some((session_id, exit)) = completed else {
            return;
        };
        let Some(sender) = self.completion.lock().unwrap().take() else {
            return;
        };
        let (stdout, truncated) = self.output.lock().unwrap().snapshot();
        let _ = sender.send(SessionOutcome::Completed {
            session_id,
            exit,
            stdout,
            truncated,
        });
    }

    fn fail(&self, message: String) {
        if let Some(sender) = self.completion.lock().unwrap().take() {
            let _ = sender.send(SessionOutcome::Failed(message));
        }
    }
}

#[derive(Clone)]
pub(crate) struct SessionCommand {
    pending: Arc<PendingSession>,
    command: String,
}

impl SessionCommand {
    pub(crate) fn command(&self) -> &str {
        &self.command
    }

    pub(crate) fn grant_id(&self) -> &str {
        &self.pending.grant_id
    }

    pub(crate) fn observe(&self, bytes: &[u8]) {
        if bytes == SSH_AUTHENTICATED_MARKER {
            self.pending.authenticated.store(true, Ordering::Release);
        } else if self.pending.authenticated.load(Ordering::Acquire) {
            self.pending.output.lock().unwrap().push(bytes);
        }
    }

    pub(crate) fn display_line(&self) -> String {
        let mut display = String::new();
        for character in self.command.chars().take(4096) {
            match character {
                '\r' => display.push_str("\\r"),
                '\n' => display.push_str("\\n"),
                character if character.is_control() => display.push('�'),
                character => display.push(character),
            }
        }
        if self.command.chars().count() > 4096 {
            display.push('…');
        }
        format!("\r\n\x1b[2mAgent command:\x1b[0m {display}\r\n")
    }

    pub(crate) fn started(&self, session_id: &str) {
        self.pending.progress.lock().unwrap().session_id = Some(session_id.to_string());
        self.pending.resolve_if_complete();
    }

    pub(crate) fn finished(&self, exit: SshExit) {
        self.pending.progress.lock().unwrap().exit = Some(exit);
        self.pending.resolve_if_complete();
    }

    pub(crate) fn failed(&self, message: String) {
        self.pending.fail(message);
    }
}

#[derive(Default)]
pub(crate) struct SessionRegistry {
    pending: Mutex<HashMap<String, Arc<PendingSession>>>,
}

impl SessionRegistry {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn request(
        &self,
        app: &AppHandle,
        grant_id: &str,
        grant_label: &str,
        host_id: &str,
        host_name: &str,
        command: &str,
        timeout: Duration,
    ) -> SessionOutcome {
        if app.webview_windows().is_empty() {
            return SessionOutcome::Unavailable;
        }

        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        let pending = Arc::new(PendingSession::new(grant_id, host_id, command, sender));
        self.pending
            .lock()
            .unwrap()
            .insert(id.clone(), Arc::clone(&pending));

        let request = SessionRequest {
            id: id.clone(),
            grant_id: grant_id.to_string(),
            grant_label: grant_label.to_string(),
            host_id: host_id.to_string(),
            host_name: host_name.to_string(),
        };
        if app.emit(SESSION_EVENT, request).is_err() {
            self.pending.lock().unwrap().remove(&id);
            return SessionOutcome::Unavailable;
        }

        let outcome = match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => SessionOutcome::Failed("Luma is shutting down".into()),
            Err(_) => SessionOutcome::TimedOut(pending.progress.lock().unwrap().session_id.clone()),
        };
        self.pending.lock().unwrap().remove(&id);
        outcome
    }

    pub(crate) fn claim(&self, request_id: &str, host_id: &str) -> Result<SessionCommand, String> {
        let pending = self
            .pending
            .lock()
            .unwrap()
            .get(request_id)
            .cloned()
            .ok_or_else(|| "the MCP command request expired".to_string())?;
        if pending.host_id != host_id {
            return Err("the MCP command request belongs to a different host".into());
        }
        let command = pending
            .command
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| "the MCP command request was already claimed".to_string())?;
        Ok(SessionCommand { pending, command })
    }

    pub(crate) fn fail(&self, request_id: &str, message: String) {
        if let Some(pending) = self.pending.lock().unwrap().get(request_id).cloned() {
            pending.fail(message);
        }
    }

    pub(crate) fn cancel_all(&self) {
        let pending: Vec<_> = self
            .pending
            .lock()
            .unwrap()
            .drain()
            .map(|(_, value)| value)
            .collect();
        for request in pending {
            request.fail("The MCP grant was revoked while its command was pending.".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(command: &str) -> (Arc<PendingSession>, oneshot::Receiver<SessionOutcome>) {
        let (sender, receiver) = oneshot::channel();
        (
            Arc::new(PendingSession::new("grant", "host", command, sender)),
            receiver,
        )
    }

    #[tokio::test]
    async fn completion_waits_for_both_session_id_and_exit() {
        let (pending, receiver) = pending("whoami");
        let command = SessionCommand {
            pending,
            command: "whoami".into(),
        };
        command.observe(SSH_AUTHENTICATED_MARKER);
        command.observe(b"user\r\n");
        command.finished(SshExit {
            code: Some(0),
            error_category: None,
            error_message: None,
        });
        command.started("session-1");

        assert_eq!(
            receiver.await.unwrap(),
            SessionOutcome::Completed {
                session_id: "session-1".into(),
                exit: SshExit {
                    code: Some(0),
                    error_category: None,
                    error_message: None,
                },
                stdout: "user\n".into(),
                truncated: false,
            }
        );
    }

    #[test]
    fn display_line_escapes_control_characters() {
        let (pending, _receiver) = pending("echo one\necho \x1b[31mtwo");
        let command = SessionCommand {
            pending,
            command: "echo one\necho \x1b[31mtwo".into(),
        };
        let display = command.display_line();
        assert!(display.contains("echo one\\necho �[31mtwo"));
        assert!(!display.contains("\x1b[31m"));
    }
}
