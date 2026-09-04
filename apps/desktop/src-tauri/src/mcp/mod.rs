//! MCP server: lets an external agent operate granted hosts and shared panes
//! through Luma, without ever holding SSH credentials.
//!
//! The agent authenticates with a grant token. Luma resolves credentials from
//! the keystore exactly as it does for a human, runs the work, and returns only
//! the result — so revoking the grant revokes the access, and nothing the agent
//! holds is useful anywhere else.
//!
//! Lifecycle is tied to grants existing: no grant, no socket. Creating the
//! first grant is the deliberate act that opens the endpoint, and deleting the
//! last one closes it.

pub(crate) mod approval;
mod grants;
mod protocol;
mod server;
pub(crate) mod sessions;
pub mod stdio;
pub(crate) mod taps;
mod tools;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Manager};
use tokio::sync::Semaphore;

use crate::errors::Result;
use crate::storage::mcp_grants::{self, McpGrant};
use crate::AppState;

pub(crate) use approval::ApprovalRegistry;
pub(crate) use grants::generate as generate_token;
pub(crate) use sessions::SessionRegistry;
pub(crate) use taps::{PaneKind, PaneTap, TapRegistry};

/// Path an MCP client should launch to reach this installation.
pub(crate) fn client_executable_path() -> Result<String> {
    // Under AppImage `current_exe` points inside the ephemeral squashfs mount,
    // which disappears when the app exits — a config written from it would work
    // until the next restart and then mysteriously stop.
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        let path = PathBuf::from(appimage);
        if path.exists() {
            return Ok(path.to_string_lossy().into_owned());
        }
    }
    Ok(std::env::current_exe()?.to_string_lossy().into_owned())
}

/// How many SSH connections one grant may have open at once. Matches the
/// snippet runner's fan-out limit: enough for an agent to work, low enough that
/// a runaway loop cannot flood a host.
const MAX_CONCURRENT_PER_GRANT: usize = 4;
pub(crate) const MAX_COMMAND_OUTPUT_BYTES: usize = 1024 * 1024;
/// Recent tool calls kept for the activity list. Bounded because this is a
/// live view, not an audit archive.
const MAX_ACTIVITY_ENTRIES: usize = 200;
/// Written next to the database so the stdio bridge can find the port. Contains
/// no secret — see `write_endpoint_file`.
const ENDPOINT_FILE: &str = "mcp-endpoint.json";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub grant_count: i64,
}

/// One entry in the activity list.
///
/// Deliberately records *shapes*, not content: how many bytes a command was,
/// never the command itself. An agent-supplied command can embed a password in
/// a heredoc or an environment assignment, and no redactor can reliably find it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEntry {
    pub id: String,
    pub at: i64,
    pub grant_id: String,
    pub grant_label: String,
    /// `"run-command"` or `"pane-send"`.
    pub action: &'static str,
    pub target: String,
    pub input_bytes: usize,
    pub exit_code: Option<u32>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
    /// Terminal session involved in the activity, if any.
    pub session_id: Option<String>,
    /// `"tab"` for visible command sessions and `"exec"` for headless fallback.
    pub via: Option<&'static str>,
}

/// Everything the activity list records about one finished command.
pub(crate) struct CommandOutcome<'a> {
    pub grant_id: &'a str,
    pub grant_label: &'a str,
    pub host_id: &'a str,
    pub host_name: &'a str,
    pub input_bytes: usize,
    pub exit_code: Option<u32>,
    pub duration_ms: u64,
    pub error: Option<String>,
    pub session_id: Option<String>,
    pub via: &'static str,
}

impl ActivityEntry {
    fn command(outcome: CommandOutcome<'_>) -> Self {
        tracing::info!(
            grant_id = outcome.grant_id,
            host_id = outcome.host_id,
            input_bytes = outcome.input_bytes,
            exit_code = outcome.exit_code,
            duration_ms = outcome.duration_ms,
            via = outcome.via,
            failed = outcome.error.is_some(),
            "MCP: ran a command"
        );
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            at: chrono::Utc::now().timestamp(),
            grant_id: outcome.grant_id.to_string(),
            grant_label: outcome.grant_label.to_string(),
            action: "run-command",
            target: outcome.host_name.to_string(),
            input_bytes: outcome.input_bytes,
            exit_code: outcome.exit_code,
            duration_ms: Some(outcome.duration_ms),
            error: outcome.error,
            session_id: outcome.session_id,
            via: Some(outcome.via),
        }
    }

    fn pane_send(grant_id: &str, grant_label: &str, pane_id: &str, input_bytes: usize) -> Self {
        tracing::info!(grant_id, pane_id, input_bytes, "MCP: sent input to a pane");
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            at: chrono::Utc::now().timestamp(),
            grant_id: grant_id.to_string(),
            grant_label: grant_label.to_string(),
            action: "pane-send",
            target: pane_id.to_string(),
            input_bytes,
            exit_code: None,
            duration_ms: None,
            error: None,
            session_id: Some(pane_id.to_string()),
            via: None,
        }
    }
}

/// State reachable from a request handler.
#[derive(Default)]
pub(crate) struct McpShared {
    pub taps: Arc<TapRegistry>,
    pub approvals: Arc<ApprovalRegistry>,
    pub sessions: Arc<SessionRegistry>,
    activity: Mutex<VecDeque<ActivityEntry>>,
    semaphores: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl McpShared {
    fn semaphore_for(&self, grant_id: &str) -> Arc<Semaphore> {
        Arc::clone(
            self.semaphores
                .lock()
                .unwrap()
                .entry(grant_id.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(MAX_CONCURRENT_PER_GRANT))),
        )
    }

    fn record(&self, entry: ActivityEntry) {
        let mut activity = self.activity.lock().unwrap();
        if activity.len() == MAX_ACTIVITY_ENTRIES {
            activity.pop_front();
        }
        activity.push_back(entry);
    }

    fn activity(&self) -> Vec<ActivityEntry> {
        self.activity
            .lock()
            .unwrap()
            .iter()
            .rev()
            .cloned()
            .collect()
    }
}

#[derive(Default)]
pub struct McpState {
    shared: Arc<McpShared>,
    server: Mutex<Option<server::RunningServer>>,
}

impl McpState {
    /// Create a tap for a session about to spawn.
    ///
    /// Called unconditionally for every session, so it must stay cheap: the
    /// tap does not allocate a buffer until the user shares the pane.
    pub fn new_tap_ssh(&self, host_id: &str) -> PaneTap {
        self.shared.taps.new_tap(PaneKind::Ssh {
            host_id: host_id.to_string(),
        })
    }

    pub fn new_tap_local(&self) -> PaneTap {
        self.shared.taps.new_tap(PaneKind::Local)
    }

    pub(crate) fn taps(&self) -> &Arc<TapRegistry> {
        &self.shared.taps
    }

    pub(crate) fn approvals(&self) -> &Arc<ApprovalRegistry> {
        &self.shared.approvals
    }

    pub(crate) fn sessions(&self) -> &Arc<SessionRegistry> {
        &self.shared.sessions
    }

    pub(crate) fn activity(&self) -> Vec<ActivityEntry> {
        self.shared.activity()
    }

    pub(crate) fn forget_grant(&self, grant_id: &str) {
        self.shared.semaphores.lock().unwrap().remove(grant_id);
    }

    fn port(&self) -> Option<u16> {
        self.server
            .lock()
            .unwrap()
            .as_ref()
            .map(|running| running.port)
    }

    pub async fn status(&self, app: &AppHandle) -> Result<McpStatus> {
        let state = app.state::<AppState>();
        let grant_count = mcp_grants::count(&state.pool).await?;
        let port = self.port();
        Ok(McpStatus {
            running: port.is_some(),
            port,
            grant_count,
        })
    }

    /// Start or stop the listener so it matches "at least one grant exists".
    ///
    /// Idempotent, and called after every change to the grant list as well as
    /// at startup.
    pub async fn sync_lifecycle(&self, app: &AppHandle) -> Result<()> {
        let state = app.state::<AppState>();
        let wanted = mcp_grants::count(&state.pool).await? > 0;
        let running = self.port().is_some();
        if wanted == running {
            return Ok(());
        }
        if wanted {
            let running = server::start(app.clone(), Arc::clone(&self.shared)).await?;
            tracing::info!(port = running.port, "MCP: listening on 127.0.0.1");
            write_endpoint_file(&state.app_data_dir, running.port)?;
            *self.server.lock().unwrap() = Some(running);
        } else {
            self.stop(&state.app_data_dir);
        }
        Ok(())
    }

    pub fn stop(&self, app_data_dir: &Path) {
        if let Some(running) = self.server.lock().unwrap().take() {
            running.stop();
        }
        self.shared.approvals.deny_all();
        self.shared.sessions.cancel_all();
        self.shared.taps.unshare_all(None);
        let _ = std::fs::remove_file(endpoint_path(app_data_dir));
    }

    /// Called when the keystore locks: a share authorised while unlocked must
    /// not outlive that state.
    pub fn revoke_all_shares(&self) {
        self.shared.taps.unshare_all(None);
        self.shared.approvals.deny_all();
        self.shared.sessions.cancel_all();
    }

    pub fn kill_all(&self) {
        if let Some(running) = self.server.lock().unwrap().take() {
            running.stop();
        }
        self.shared.approvals.deny_all();
        self.shared.sessions.cancel_all();
        self.shared.taps.unshare_all(None);
    }
}

/// Resolve a presented bearer token to the grant it belongs to.
async fn authenticate(app: &AppHandle, token: &str) -> Result<Option<McpGrant>> {
    let state = app.state::<AppState>();
    let presented = grants::digest(token);
    let Some(grant) = mcp_grants::find_by_token_hash(&state.pool, &presented).await? else {
        // Never log the token, in any form: the redactor only catches it when
        // it appears as a recognised key/value pair, and a bare positional
        // field would sail straight through.
        tracing::warn!("MCP: rejected a request with an unrecognised token");
        return Ok(None);
    };
    // The lookup already matched; confirming in constant time costs nothing and
    // removes the question of whether the index leaked timing.
    if !grants::constant_time_eq(&presented, &grants::digest(token)) {
        return Ok(None);
    }
    Ok(Some(grant))
}

fn endpoint_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(ENDPOINT_FILE)
}

/// Publish the bound port for the stdio bridge to find.
///
/// **Contains no secret.** The token travels through the agent's own config, so
/// this file staying readable is not a credential exposure — and it must stay
/// that way, because Luma sets no Windows ACLs and the `0600` below is a no-op
/// there. Do not put a token in here.
fn write_endpoint_file(app_data_dir: &Path, port: u16) -> Result<()> {
    let path = endpoint_path(app_data_dir);
    let temporary = path.with_extension("json.tmp");
    let body = serde_json::json!({
        "version": 1,
        "port": port,
        "pid": std::process::id(),
    });

    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&temporary)?;
        serde_json::to_writer(&file, &body).map_err(|error| {
            crate::errors::LumaError::Pty(format!("could not write the MCP endpoint file: {error}"))
        })?;
        file.sync_all()?;
    }
    // Rename so a bridge starting concurrently never reads a half-written file.
    std::fs::rename(&temporary, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_is_bounded_and_newest_first() {
        let shared = McpShared::default();
        for index in 0..(MAX_ACTIVITY_ENTRIES + 10) {
            shared.record(ActivityEntry::pane_send(
                "grant",
                "Agent",
                &format!("pane-{index}"),
                index,
            ));
        }
        let activity = shared.activity();
        assert_eq!(activity.len(), MAX_ACTIVITY_ENTRIES);
        // Newest first, and the oldest entries were dropped rather than the new
        // ones being refused.
        assert_eq!(
            activity[0].target,
            format!("pane-{}", MAX_ACTIVITY_ENTRIES + 9)
        );
        assert_eq!(activity[MAX_ACTIVITY_ENTRIES - 1].target, "pane-10");
    }

    #[test]
    fn activity_records_shapes_not_content() {
        let entry = ActivityEntry::command(CommandOutcome {
            grant_id: "grant",
            grant_label: "Agent",
            host_id: "host-1",
            host_name: "prod",
            input_bytes: 42,
            exit_code: Some(0),
            duration_ms: 17,
            error: None,
            session_id: None,
            via: "exec",
        });
        assert_eq!(entry.action, "run-command");
        assert_eq!(entry.target, "prod");
        assert_eq!(entry.input_bytes, 42);
        assert_eq!(entry.via, Some("exec"));
        assert_eq!(entry.session_id, None);
        // The command itself is nowhere in the serialised entry.
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("command\":\""));
    }

    #[test]
    fn each_grant_gets_its_own_concurrency_budget() {
        let shared = McpShared::default();
        let first = shared.semaphore_for("grant-a");
        assert!(Arc::ptr_eq(&first, &shared.semaphore_for("grant-a")));
        assert!(!Arc::ptr_eq(&first, &shared.semaphore_for("grant-b")));
        assert_eq!(first.available_permits(), MAX_CONCURRENT_PER_GRANT);
    }

    #[test]
    fn deleting_a_grant_drops_its_concurrency_budget() {
        let state = McpState::default();
        let held = state.shared.semaphore_for("grant-a");
        state.forget_grant("grant-a");
        let replacement = state.shared.semaphore_for("grant-a");

        assert!(!Arc::ptr_eq(&held, &replacement));
        assert_eq!(state.shared.semaphores.lock().unwrap().len(), 1);
    }

    #[test]
    fn the_endpoint_file_carries_no_secret() {
        let directory = std::env::temp_dir().join(format!("luma-mcp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        write_endpoint_file(&directory, 51234).unwrap();

        let body = std::fs::read_to_string(endpoint_path(&directory)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["port"], 51234);
        assert_eq!(parsed["version"], 1);
        assert!(parsed["pid"].is_number());
        assert_eq!(parsed.as_object().unwrap().len(), 3);
        for forbidden in ["token", "secret", "luma_mcp_"] {
            assert!(
                !body.contains(forbidden),
                "endpoint file mentions {forbidden}"
            );
        }

        // Rewriting is atomic and leaves no temp file behind.
        write_endpoint_file(&directory, 51235).unwrap();
        assert!(!directory.join("mcp-endpoint.json.tmp").exists());

        std::fs::remove_dir_all(&directory).unwrap();
    }
}
