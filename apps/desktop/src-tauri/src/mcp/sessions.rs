//! Command-specific interactive SSH sessions requested through the webview.
//!
//! The frontend owns terminal creation, while the MCP request owns the command.
//! A random request id joins those two halves without ever sending the command
//! through React.
//!
//! The tab outlives its first command: the grant's later commands for the same
//! host run in it too, until the agent or the user closes it. On most hosts it
//! is an ordinary shell the agent shares with the user, so working directory
//! and environment carry over and either can type at any time; completion is
//! read from a per-command marker (see `super::sentinel`). Windows shells
//! cannot run that marker's helper, so there each command runs as its own exec
//! channel on the tab's connection and completes on the SSH exit status.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;

use crate::mcp::sentinel::{strip_echo, Sentinel};
use crate::mcp::taps::strip_terminal_output;
use crate::mcp::MAX_COMMAND_OUTPUT_BYTES;
use crate::ssh::{EmbeddedSshManager, SshExit};

pub(crate) const SESSION_EVENT: &str = "mcp-session-request";

/// How long an agent's tab stays connected with no command running. Long
/// enough to cover an agent reading output and deciding what to run next.
pub(crate) const AGENT_TAB_IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

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

    /// Whether `needle` occurs in the last `window` bytes. Lets a large output
    /// be scanned per chunk without re-stripping all of it each time.
    fn tail_contains(&mut self, needle: &[u8], window: usize) -> bool {
        let bytes = self.bytes.make_contiguous();
        let tail = &bytes[bytes.len().saturating_sub(window)..];
        tail.windows(needle.len())
            .any(|candidate| candidate == needle)
    }
}

/// How an agent tab runs commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TabMode {
    /// Typed into an interactive shell the agent shares with the user, so
    /// shell state carries over. Completion comes from [`Sentinel`].
    Shell,
    /// One SSH exec channel per command, completing on its exit status. For
    /// Windows hosts, whose shells cannot run the sentinel helper.
    Exec,
}

#[derive(Default)]
struct Progress {
    session_id: Option<String>,
    exit: Option<SshExit>,
}

struct PendingSession {
    grant_id: String,
    host_id: String,
    mode: TabMode,
    command: Mutex<Option<String>>,
    capturing: AtomicBool,
    output: Mutex<CapturedOutput>,
    progress: Mutex<Progress>,
    completion: Mutex<Option<oneshot::Sender<SessionOutcome>>>,
}

impl PendingSession {
    fn new(
        grant_id: &str,
        host_id: &str,
        mode: TabMode,
        command: &str,
        completion: oneshot::Sender<SessionOutcome>,
    ) -> Self {
        Self {
            grant_id: grant_id.to_string(),
            host_id: host_id.to_string(),
            mode,
            command: Mutex::new(Some(command.to_string())),
            capturing: AtomicBool::new(false),
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
        self.resolve(SessionOutcome::Failed(message));
    }

    fn resolve(&self, outcome: SessionOutcome) {
        if let Some(sender) = self.completion.lock().unwrap().take() {
            let _ = sender.send(outcome);
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

    pub(crate) fn mode(&self) -> TabMode {
        self.pending.mode
    }

    /// The command is running, so what follows on the data channel is its own
    /// output rather than the connection getting established.
    fn begin_capture(&self) {
        self.pending.capturing.store(true, Ordering::Release);
    }

    pub(crate) fn observe(&self, bytes: &[u8]) {
        if self.pending.capturing.load(Ordering::Acquire) {
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

/// The terminal an agent tab types into. A trait so the tab's command flow can
/// be tested without a live SSH session.
pub(crate) trait TabTerminal: Send + Sync {
    fn write(&self, session_id: &str, text: String) -> bool;
    fn disconnect(&self, session_id: &str);
}

/// [`TabTerminal`] over the app's embedded SSH sessions.
pub(crate) struct EmbeddedTerminal(pub(crate) AppHandle);

impl TabTerminal for EmbeddedTerminal {
    fn write(&self, session_id: &str, text: String) -> bool {
        self.0
            .state::<EmbeddedSshManager>()
            .write(session_id, text)
            .unwrap_or(false)
    }

    fn disconnect(&self, session_id: &str) {
        let _ = self.0.state::<EmbeddedSshManager>().disconnect(session_id);
    }
}

/// The shell-mode command that has been typed and is waiting for its marker.
struct ShellRun {
    pending: Arc<PendingSession>,
    token: String,
    needle: String,
    typed: String,
    marker_seen: bool,
}

struct ShellState {
    sentinel: Sentinel,
    helper_defined: bool,
    /// The shell is open and the remote OS is known not to be Windows, so a
    /// typed line reaches a POSIX shell rather than the auth prompt.
    ready: bool,
    run: Option<ShellRun>,
}

/// An agent's command tab. It stays connected after its command finishes, and
/// the grant's next command for the same host runs in it instead of opening
/// another tab. See [`TabMode`] for how each command runs.
pub(crate) struct AgentTab {
    grant_id: String,
    host_id: String,
    mode: TabMode,
    terminal: Box<dyn TabTerminal>,
    session_id: OnceLock<String>,
    /// The command running now; `None` while the tab is idle. Lock order:
    /// `shell` before `current`.
    current: Mutex<Option<SessionCommand>>,
    shell: Mutex<ShellState>,
    closed: AtomicBool,
}

impl AgentTab {
    pub(crate) fn new(first: SessionCommand, terminal: Box<dyn TabTerminal>) -> Arc<Self> {
        Arc::new(Self {
            grant_id: first.pending.grant_id.clone(),
            host_id: first.pending.host_id.clone(),
            mode: first.mode(),
            terminal,
            session_id: OnceLock::new(),
            current: Mutex::new(Some(first)),
            shell: Mutex::new(ShellState {
                sentinel: Sentinel::new(&uuid::Uuid::new_v4().simple().to_string()),
                helper_defined: false,
                ready: false,
                run: None,
            }),
            closed: AtomicBool::new(false),
        })
    }

    /// Remote OS detection finished, which happens only once the shell is
    /// open. A Windows shell cannot run the sentinel helper, so the command is
    /// handed back to run headless, and the tab, opened only for the agent,
    /// is closed.
    pub(crate) fn remote_os_detected(&self, os_id: &str) {
        if self.mode != TabMode::Shell {
            return;
        }
        if os_id == "windows" {
            self.closed.store(true, Ordering::Release);
            if let Some(command) = self.current.lock().unwrap().take() {
                command.pending.resolve(SessionOutcome::Unavailable);
            }
            if let Some(session_id) = self.session_id.get() {
                self.terminal.disconnect(session_id);
            }
            return;
        }
        self.shell.lock().unwrap().ready = true;
        self.submit();
    }

    /// Types the current shell-mode command, once the shell is ready and the
    /// tab knows its session.
    fn submit(&self) {
        if self.mode != TabMode::Shell {
            return;
        }
        let mut shell = self.shell.lock().unwrap();
        if !shell.ready || shell.run.is_some() {
            return;
        }
        let Some(session_id) = self.session_id.get() else {
            return;
        };
        let Some(command) = self.current() else {
            return;
        };
        let token = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let typed = shell
            .sentinel
            .command_line(command.command(), &token, !shell.helper_defined);
        command.begin_capture();
        if !self.terminal.write(session_id, typed.clone()) {
            drop(shell);
            if let Some(command) = self.current.lock().unwrap().take() {
                command.failed("Luma could not type the command into its terminal tab.".into());
            }
            return;
        }
        shell.helper_defined = true;
        shell.run = Some(ShellRun {
            pending: Arc::clone(&command.pending),
            needle: shell.sentinel.needle(&token),
            token,
            typed,
            marker_seen: false,
        });
    }

    /// Stops waiting for a shell-mode command that ran out of time, so the tab
    /// takes the next one. The command may still be running; its late marker
    /// carries its own token and cannot complete a later command.
    fn abandon(&self, pending: &Arc<PendingSession>) {
        if self.mode != TabMode::Shell {
            return;
        }
        let mut shell = self.shell.lock().unwrap();
        if !shell
            .run
            .as_ref()
            .is_some_and(|run| Arc::ptr_eq(&run.pending, pending))
        {
            return;
        }
        shell.run = None;
        // The command may have replaced the shell (`exec`, `su`), taking the
        // helper with it. Defining it again is harmless.
        shell.helper_defined = false;
        drop(shell);
        self.release(pending);
    }

    fn observe_shell(&self, bytes: &[u8]) {
        let mut guard = self.shell.lock().unwrap();
        let shell = &mut *guard;
        let Some(run) = shell.run.as_mut() else {
            return;
        };
        let mut output = run.pending.output.lock().unwrap();
        output.push(bytes);
        if !run.marker_seen {
            let window = bytes.len() + run.needle.len();
            run.marker_seen = output.tail_contains(run.needle.as_bytes(), window);
        }
        if !run.marker_seen {
            return;
        }
        let (text, truncated) = output.snapshot();
        drop(output);
        let Some((before, code)) = shell.sentinel.parse(&text, &run.token) else {
            return;
        };
        let stdout = strip_echo(before, &run.typed).to_string();
        let Some(run) = shell.run.take() else {
            return;
        };
        let session_id = self.session_id.get().cloned().unwrap_or_default();
        run.pending.resolve(SessionOutcome::Completed {
            session_id,
            exit: SshExit {
                code,
                error_category: None,
                error_message: None,
            },
            stdout,
            truncated,
        });
        drop(guard);
        self.release(&run.pending);
    }

    pub(crate) fn grant_id(&self) -> &str {
        &self.grant_id
    }

    fn current(&self) -> Option<SessionCommand> {
        self.current.lock().unwrap().clone()
    }

    /// Starts capturing the current command's output and returns the line that
    /// shows the user which command is about to run.
    pub(crate) fn command_started(&self) -> Option<String> {
        let command = self.current()?;
        command.begin_capture();
        Some(command.display_line())
    }

    pub(crate) fn observe(&self, bytes: &[u8]) {
        match self.mode {
            TabMode::Shell => self.observe_shell(bytes),
            TabMode::Exec => {
                if let Some(command) = self.current() {
                    command.observe(bytes);
                }
            }
        }
    }

    pub(crate) fn command_finished(&self, exit: SshExit) {
        if let Some(command) = self.current.lock().unwrap().take() {
            command.finished(exit);
        }
    }

    /// The tab's SSH session ended, taking any running command with it.
    pub(crate) fn closed(&self, exit: SshExit) {
        self.closed.store(true, Ordering::Release);
        self.shell.lock().unwrap().run = None;
        self.command_finished(exit);
    }

    pub(crate) fn failed(&self, message: String) {
        self.closed.store(true, Ordering::Release);
        if let Some(command) = self.current.lock().unwrap().take() {
            command.failed(message);
        }
    }

    pub(crate) fn started(&self, session_id: &str) {
        let _ = self.session_id.set(session_id.to_string());
        if let Some(command) = self.current() {
            command.started(session_id);
        }
        // Detection usually finishes after this, but nothing guarantees it.
        self.submit();
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Claims an idle tab for `command`. False while another command runs.
    fn assign(&self, command: SessionCommand) -> bool {
        let mut current = self.current.lock().unwrap();
        if current.is_some() || self.is_closed() {
            return false;
        }
        *current = Some(command);
        drop(current);
        self.submit();
        true
    }

    /// Gives back a tab whose exec request was refused, unless something else
    /// already resolved the command.
    fn release(&self, pending: &Arc<PendingSession>) {
        let mut current = self.current.lock().unwrap();
        if current
            .as_ref()
            .is_some_and(|command| Arc::ptr_eq(&command.pending, pending))
        {
            *current = None;
        }
    }
}

#[derive(Default)]
pub(crate) struct SessionRegistry {
    pending: Mutex<HashMap<String, Arc<PendingSession>>>,
    tabs: Mutex<Vec<Arc<AgentTab>>>,
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
        mode: TabMode,
        command: &str,
        timeout: Duration,
    ) -> SessionOutcome {
        if app.webview_windows().is_empty() {
            return SessionOutcome::Unavailable;
        }
        if let Some(outcome) = self
            .run_in_open_tab(app, grant_id, host_id, command, timeout)
            .await
        {
            return outcome;
        }

        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        let pending = Arc::new(PendingSession::new(
            grant_id, host_id, mode, command, sender,
        ));
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
            Err(_) => {
                self.abandon(&pending);
                SessionOutcome::TimedOut(pending.progress.lock().unwrap().session_id.clone())
            }
        };
        self.pending.lock().unwrap().remove(&id);
        outcome
    }

    /// Runs `command` in an idle tab this grant already has open on the host.
    /// `None` when there is no such tab, so the caller opens a new one.
    async fn run_in_open_tab(
        &self,
        app: &AppHandle,
        grant_id: &str,
        host_id: &str,
        command: &str,
        timeout: Duration,
    ) -> Option<SessionOutcome> {
        let candidates: Vec<_> = self
            .tabs
            .lock()
            .unwrap()
            .iter()
            .filter(|tab| tab.grant_id == grant_id && tab.host_id == host_id)
            .cloned()
            .collect();
        let embedded = app.state::<EmbeddedSshManager>();
        for tab in candidates {
            let Some(session_id) = tab.session_id.get().cloned() else {
                continue;
            };
            let (sender, receiver) = oneshot::channel();
            let pending = Arc::new(PendingSession::new(
                grant_id, host_id, tab.mode, command, sender,
            ));
            let session_command = SessionCommand {
                pending: Arc::clone(&pending),
                command: command.to_string(),
            };
            session_command.started(&session_id);
            if !tab.assign(session_command) {
                continue;
            }

            let started = Instant::now();
            // A shell-mode tab typed the command as part of `assign`.
            if tab.mode == TabMode::Exec {
                match tokio::time::timeout(timeout, embedded.exec(&session_id, command)).await {
                    Ok(true) => {}
                    Ok(false) => {
                        tab.release(&pending);
                        continue;
                    }
                    Err(_) => return Some(SessionOutcome::TimedOut(Some(session_id))),
                }
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            return Some(match tokio::time::timeout(remaining, receiver).await {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(_)) => SessionOutcome::Failed("Luma is shutting down".into()),
                Err(_) => {
                    tab.abandon(&pending);
                    SessionOutcome::TimedOut(Some(session_id))
                }
            });
        }
        None
    }

    fn abandon(&self, pending: &Arc<PendingSession>) {
        let tabs: Vec<_> = self.tabs.lock().unwrap().clone();
        for tab in tabs {
            tab.abandon(pending);
        }
    }

    /// Makes a freshly spawned tab available for the grant's later commands.
    pub(crate) fn register_tab(&self, tab: &Arc<AgentTab>) {
        let mut tabs = self.tabs.lock().unwrap();
        // Its session may already have ended before the spawn returned.
        if !tab.is_closed() {
            tabs.push(Arc::clone(tab));
        }
    }

    pub(crate) fn remove_tab(&self, tab: &Arc<AgentTab>) {
        self.tabs
            .lock()
            .unwrap()
            .retain(|candidate| !Arc::ptr_eq(candidate, tab));
    }

    /// Stops reusing this grant's tabs on `host_id` and returns their session
    /// ids so the caller can disconnect them.
    pub(crate) fn close_tabs(&self, grant_id: &str, host_id: &str) -> Vec<String> {
        let mut closed = Vec::new();
        self.tabs.lock().unwrap().retain(|tab| {
            let matches = tab.grant_id == grant_id && tab.host_id == host_id;
            if matches {
                closed.extend(tab.session_id.get().cloned());
            }
            !matches
        });
        closed
    }

    /// A deleted grant's tabs are never reused. They stay on screen for the
    /// user, who can keep using or close them.
    pub(crate) fn forget_grant(&self, grant_id: &str) {
        self.tabs
            .lock()
            .unwrap()
            .retain(|tab| tab.grant_id != grant_id);
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
        // Revoked access must not carry on through a tab opened before it.
        // The tabs themselves stay open; they are the user's terminals too.
        self.tabs.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what a tab types and which sessions it closes.
    #[derive(Default)]
    struct FakeTerminal {
        writes: Mutex<Vec<String>>,
        disconnected: Mutex<Vec<String>>,
    }

    impl TabTerminal for Arc<FakeTerminal> {
        fn write(&self, _session_id: &str, text: String) -> bool {
            self.writes.lock().unwrap().push(text);
            true
        }

        fn disconnect(&self, session_id: &str) {
            self.disconnected
                .lock()
                .unwrap()
                .push(session_id.to_string());
        }
    }

    impl FakeTerminal {
        fn last_write(&self) -> String {
            self.writes
                .lock()
                .unwrap()
                .last()
                .cloned()
                .unwrap_or_default()
        }
    }

    fn pending(
        command: &str,
        mode: TabMode,
    ) -> (Arc<PendingSession>, oneshot::Receiver<SessionOutcome>) {
        let (sender, receiver) = oneshot::channel();
        (
            Arc::new(PendingSession::new("grant", "host", mode, command, sender)),
            receiver,
        )
    }

    fn command(text: &str, mode: TabMode) -> (SessionCommand, oneshot::Receiver<SessionOutcome>) {
        let (pending, receiver) = pending(text, mode);
        (
            SessionCommand {
                pending,
                command: text.into(),
            },
            receiver,
        )
    }

    fn tab(first: SessionCommand) -> (Arc<AgentTab>, Arc<FakeTerminal>) {
        let terminal = Arc::new(FakeTerminal::default());
        (
            AgentTab::new(first, Box::new(Arc::clone(&terminal))),
            terminal,
        )
    }

    fn exit(code: u32) -> SshExit {
        SshExit {
            code: Some(code),
            error_category: None,
            error_message: None,
        }
    }

    /// What a shell sends back for a typed line: prompt and echo, the output,
    /// then the helper's marker line and the escape that erases it.
    fn shell_reply(tab: &AgentTab, typed: &str, output: &str, code: u32) -> Vec<u8> {
        let line = typed.trim_end_matches('\r');
        let token = line.rsplit(' ').next().unwrap();
        let needle = tab.shell.lock().unwrap().sentinel.needle(token);
        let echo = line.trim_start_matches('\u{15}');
        format!("user@host:~$ {echo}\r\n{output}\r\n{needle}{code}\r\n\x1b[1A\r\x1b[2K")
            .into_bytes()
    }

    #[tokio::test]
    async fn completion_waits_for_both_session_id_and_exit() {
        let (command, receiver) = command("whoami", TabMode::Exec);
        command.begin_capture();
        command.observe(b"user\r\n");
        command.finished(exit(0));
        command.started("session-1");

        assert_eq!(
            receiver.await.unwrap(),
            SessionOutcome::Completed {
                session_id: "session-1".into(),
                exit: exit(0),
                stdout: "user\n".into(),
                truncated: false,
            }
        );
    }

    #[tokio::test]
    async fn a_shell_tab_types_once_ready_and_completes_on_its_marker() {
        let (first, first_done) = command("cd /srv", TabMode::Shell);
        let (tab, terminal) = tab(first);
        tab.started("session-1");
        // The shell is not known to be open yet, so nothing may be typed: it
        // could land in a password prompt.
        assert!(terminal.writes.lock().unwrap().is_empty());

        tab.observe(b"Welcome to the host\r\n");
        tab.remote_os_detected("ubuntu");
        let typed = terminal.last_write();
        assert!(
            typed.starts_with("\u{15}__luma() {"),
            "the first line defines the helper"
        );
        assert!(typed.contains("; cd /srv; __luma "));

        tab.observe(&shell_reply(&tab, &typed, "", 0));
        let SessionOutcome::Completed { exit, stdout, .. } = first_done.await.unwrap() else {
            panic!("expected the command to complete");
        };
        assert_eq!(exit.code, Some(0));
        assert!(
            !stdout.contains("Welcome"),
            "output before typing is not captured"
        );
        assert!(stdout.trim().is_empty());

        // Same shell: no second definition, and the status and output come
        // back without the prompt or the echo.
        let (second, second_done) = command("ls missing", TabMode::Shell);
        second.started("session-1");
        assert!(tab.assign(second));
        let typed = terminal.last_write();
        assert!(typed.starts_with("\u{15}ls missing; __luma "));
        tab.observe(&shell_reply(&tab, &typed, "ls: missing: No such file", 2));
        let SessionOutcome::Completed { exit, stdout, .. } = second_done.await.unwrap() else {
            panic!("expected the command to complete");
        };
        assert_eq!(exit.code, Some(2));
        assert_eq!(stdout.trim(), "ls: missing: No such file");
    }

    #[tokio::test]
    async fn a_busy_shell_tab_is_not_reused() {
        let (first, _first_done) = command("sleep 100", TabMode::Shell);
        let (tab, _terminal) = tab(first);
        tab.started("session-1");
        tab.remote_os_detected("debian");

        let (second, _second_done) = command("uptime", TabMode::Shell);
        assert!(!tab.assign(second));
    }

    /// A timed-out command keeps running and prints its marker later, into the
    /// next command's output. That marker must not complete the next command.
    #[tokio::test]
    async fn an_abandoned_commands_late_marker_does_not_complete_the_next_one() {
        let (first, _first_done) = command("sleep 100", TabMode::Shell);
        let first_pending = Arc::clone(&first.pending);
        let (tab, terminal) = tab(first);
        tab.started("session-1");
        tab.remote_os_detected("debian");
        let first_typed = terminal.last_write();
        tab.abandon(&first_pending);

        let (second, mut second_done) = command("uptime", TabMode::Shell);
        second.started("session-1");
        assert!(tab.assign(second));
        let second_typed = terminal.last_write();
        // The abandoned command may have replaced the shell, so the helper is
        // defined again.
        assert!(second_typed.starts_with("\u{15}__luma() {"));

        tab.observe(&shell_reply(&tab, &first_typed, "", 0));
        assert!(second_done.try_recv().is_err());

        tab.observe(&shell_reply(&tab, &second_typed, "up 3 days", 0));
        let SessionOutcome::Completed { stdout, .. } = second_done.await.unwrap() else {
            panic!("expected the command to complete");
        };
        assert!(stdout.contains("up 3 days"));
    }

    #[tokio::test]
    async fn a_windows_host_hands_the_command_back_and_closes_its_shell_tab() {
        let (first, first_done) = command("dir", TabMode::Shell);
        let (tab, terminal) = tab(first);
        tab.started("session-1");
        tab.remote_os_detected("windows");

        assert_eq!(first_done.await.unwrap(), SessionOutcome::Unavailable);
        assert!(terminal.writes.lock().unwrap().is_empty());
        assert_eq!(*terminal.disconnected.lock().unwrap(), vec!["session-1"]);
        let (next, _next_done) = command("dir", TabMode::Shell);
        assert!(!tab.assign(next));
    }

    #[tokio::test]
    async fn an_exec_tab_takes_a_new_command_only_once_the_last_one_finished() {
        let (first, first_done) = command("uptime", TabMode::Exec);
        let (tab, terminal) = tab(first);
        tab.started("session-1");
        assert!(tab.command_started().unwrap().contains("uptime"));
        tab.observe(b"up 3 days\r\n");

        let (second, second_done) = command("whoami", TabMode::Exec);
        second.started("session-1");
        assert!(!tab.assign(second.clone()), "a busy tab must not be reused");

        tab.command_finished(exit(0));
        assert!(matches!(
            first_done.await.unwrap(),
            SessionOutcome::Completed { stdout, .. } if stdout == "up 3 days\n"
        ));

        assert!(tab.assign(second));
        tab.command_started();
        tab.observe(b"user\r\n");
        tab.command_finished(exit(1));
        assert_eq!(
            second_done.await.unwrap(),
            SessionOutcome::Completed {
                session_id: "session-1".into(),
                exit: exit(1),
                stdout: "user\n".into(),
                truncated: false,
            }
        );
        // Exec tabs run commands on their own channels; nothing is typed.
        assert!(terminal.writes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn closing_a_tab_resolves_its_running_command_and_stops_reuse() {
        let (first, first_done) = command("sleep 100", TabMode::Shell);
        let (tab, _terminal) = tab(first);
        tab.started("session-1");
        tab.remote_os_detected("debian");
        let registry = SessionRegistry::default();
        registry.register_tab(&tab);

        tab.closed(SshExit {
            code: None,
            error_category: None,
            error_message: None,
        });
        assert!(matches!(
            first_done.await.unwrap(),
            SessionOutcome::Completed {
                exit: SshExit { code: None, .. },
                ..
            }
        ));
        let (next, _next_done) = command("uptime", TabMode::Shell);
        assert!(!tab.assign(next));

        // A spawn that returns after its session already ended registers nothing.
        let registry_after = SessionRegistry::default();
        registry_after.register_tab(&tab);
        assert!(registry_after.close_tabs("grant", "host").is_empty());
    }

    #[test]
    fn close_tabs_only_takes_the_grants_tabs_on_that_host() {
        let registry = SessionRegistry::default();
        let (first, _first_done) = command("uptime", TabMode::Shell);
        let (tab, _terminal) = tab(first);
        tab.started("session-1");
        registry.register_tab(&tab);

        assert!(registry.close_tabs("other-grant", "host").is_empty());
        assert!(registry.close_tabs("grant", "other-host").is_empty());
        assert_eq!(registry.close_tabs("grant", "host"), vec!["session-1"]);
        assert!(registry.close_tabs("grant", "host").is_empty());
    }

    #[test]
    fn display_line_escapes_control_characters() {
        let (command, _receiver) = command("echo one\necho \x1b[31mtwo", TabMode::Exec);
        let display = command.display_line();
        assert!(display.contains("echo one\\necho �[31mtwo"));
        assert!(!display.contains("\x1b[31m"));
    }
}
