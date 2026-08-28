//! The four MCP tools.
//!
//! Every tool re-derives what the grant may reach from the grant itself. Ids
//! arriving over the wire are validated and checked for membership, never
//! trusted — the agent is outside the trust boundary even though it presented a
//! valid token.
//!
//! Failures come back as `isError` tool results rather than JSON-RPC errors: a
//! model can read and act on the former ("the vault is locked, ask the user to
//! unlock it") and never sees the latter.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::keystore::{self, KeystoreState};
use crate::mcp::sessions::SessionOutcome;
use crate::mcp::taps::PaneKind;
use crate::mcp::{protocol, ActivityEntry, CommandOutcome, McpShared, MAX_COMMAND_OUTPUT_BYTES};
use crate::ssh::exec::{self, ExecMessages, ExecRequest, ExecStream};
use crate::ssh::EmbeddedSshManager;
use crate::storage::hosts;
use crate::storage::mcp_grants::McpGrant;
use crate::terminal::PtyManager;
use crate::AppState;

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 600;
const MAX_COMMAND_BYTES: usize = 64 * 1024;
/// A pane is a keyboard, not a file transfer. Far below the terminal layer's
/// own 1 MiB input cap, and enough for any command someone would type.
const MAX_PANE_SEND_BYTES: usize = 8 * 1024;
const DEFAULT_PANE_READ_BYTES: usize = 64 * 1024;
const MAX_PANE_READ_BYTES: usize = 256 * 1024;
const MAX_PANE_READ_WAIT_MS: u64 = 30_000;
const EXEC_MESSAGES: ExecMessages = ExecMessages {
    timed_out: "Command timed out",
    cancelled: "Command cancelled",
    channel_open_timed_out: "SSH exec channel open timed out",
};

type ToolResult = std::result::Result<Value, String>;

pub(crate) async fn call(
    app: &AppHandle,
    shared: &Arc<McpShared>,
    grant: &McpGrant,
    name: &str,
    arguments: Value,
    id: Value,
) -> Value {
    let outcome = match name {
        protocol::TOOL_LIST_TARGETS => list_targets(app, shared, grant).await,
        protocol::TOOL_RUN_COMMAND => run_command(app, shared, grant, &arguments).await,
        protocol::TOOL_PANE_SEND => pane_send(app, shared, grant, &arguments).await,
        protocol::TOOL_PANE_READ => pane_read(shared, grant, &arguments).await,
        other => Err(format!("unknown tool: {other}")),
    };

    let state = app.state::<AppState>();
    if let Err(error) = crate::storage::mcp_grants::touch_last_used(&state.pool, &grant.id).await {
        tracing::warn!(grant_id = %grant.id, error = %error, "MCP: could not record grant use");
    }

    match outcome {
        Ok(value) => protocol::tool_result(id, value),
        Err(message) => protocol::tool_failure(id, &message),
    }
}

// --- argument helpers ---------------------------------------------------

fn required_str(arguments: &Value, key: &str) -> std::result::Result<String, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{key} is required and must be a string"))
}

fn optional_u64(
    arguments: &Value,
    key: &str,
    max: u64,
) -> std::result::Result<Option<u64>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let number = value
                .as_u64()
                .ok_or_else(|| format!("{key} must be a non-negative integer"))?;
            if number > max {
                return Err(format!("{key} must be at most {max}"));
            }
            Ok(Some(number))
        }
    }
}

/// Confirm the host id is well-formed *and* inside this grant.
///
/// The same message covers "no such host" and "not granted": which hosts exist
/// outside the grant is not something the agent needs to learn.
fn authorize_host(grant: &McpGrant, host_id: &str) -> std::result::Result<(), String> {
    crate::ssh::validate_host_id(host_id).map_err(|_| "invalid hostId".to_string())?;
    if !grant.host_ids.iter().any(|granted| granted == host_id) {
        return Err(format!(
            "host {host_id} is not available to this grant. Call luma_list_targets \
for the hosts you can reach, or ask the user to add it in Luma."
        ));
    }
    Ok(())
}

fn authorize_pane(
    shared: &McpShared,
    grant: &McpGrant,
    pane_id: &str,
) -> std::result::Result<(), String> {
    crate::ssh::validate_host_id(pane_id).map_err(|_| "invalid paneId".to_string())?;
    if !shared.taps.is_shared_with(pane_id, &grant.id) {
        return Err(format!(
            "pane {pane_id} is not shared with this grant. Ask the user to share the \
terminal tab with the agent in Luma, then call luma_list_targets again."
        ));
    }
    Ok(())
}

// --- tools --------------------------------------------------------------

async fn list_targets(app: &AppHandle, shared: &Arc<McpShared>, grant: &McpGrant) -> ToolResult {
    let state = app.state::<AppState>();
    let mut granted = Vec::with_capacity(grant.host_ids.len());
    for host_id in &grant.host_ids {
        // A host deleted since the grant was written simply drops out.
        let Ok(Some(host)) = hosts::get(&state.pool, host_id).await else {
            continue;
        };
        granted.push(json!({
            "id": host.id,
            "name": host.name,
            "hostname": host.hostname,
            "port": host.port,
            "username": host.username,
            "osPrettyName": host.os_pretty_name,
        }));
    }

    let panes: Vec<Value> = shared
        .taps
        .shared_panes(Some(&grant.id))
        .into_iter()
        .map(|pane| {
            json!({
                "id": pane.session_id,
                "title": pane.title,
                "kind": pane.kind,
                "hostId": pane.host_id,
            })
        })
        .collect();

    Ok(json!({
        "hosts": granted,
        "panes": panes,
        "requiresApproval": grant.require_approval,
    }))
}

async fn run_command(
    app: &AppHandle,
    shared: &Arc<McpShared>,
    grant: &McpGrant,
    arguments: &Value,
) -> ToolResult {
    let host_id = required_str(arguments, "hostId")?;
    let command = required_str(arguments, "command")?;
    authorize_host(grant, &host_id)?;

    if command.trim().is_empty() || command.contains('\0') || command.len() > MAX_COMMAND_BYTES {
        return Err(format!(
            "command must be non-empty, at most {MAX_COMMAND_BYTES} bytes, and contain no null characters"
        ));
    }
    let timeout_secs = optional_u64(arguments, "timeoutSeconds", MAX_TIMEOUT_SECS)?
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .max(1);

    let state = app.state::<AppState>();
    let keystore_state = app.state::<KeystoreState>();
    // Checked up front rather than per host: failing halfway through a task
    // with "works for A, not B" is worse than failing clearly, and the
    // per-host variant would leak which hosts have stored credentials.
    if !keystore::is_unlocked(&keystore_state) {
        return Err(
            "Luma's credential vault is locked, so stored SSH credentials cannot be read. \
Ask the user to unlock Luma, then retry."
                .into(),
        );
    }
    exec::validate_hosts(&state.pool, std::slice::from_ref(&host_id))
        .await
        .map_err(|error| error.to_string())?;

    let host_name = hosts::get(&state.pool, &host_id)
        .await
        .ok()
        .flatten()
        .map(|host| host.name)
        .unwrap_or_else(|| host_id.clone());

    if grant.require_approval
        && !shared
            .approvals
            .request(
                app,
                &grant.id,
                &grant.label,
                "run-command",
                &host_name,
                &command,
            )
            .await
    {
        return Err("The user denied this command in Luma.".into());
    }

    // Bounds how many connections one agent can open at once; an agent looping
    // over hosts would otherwise be indistinguishable from a connection flood.
    let semaphore = shared.semaphore_for(&grant.id);
    let _permit = semaphore
        .acquire()
        .await
        .map_err(|_| "Luma is shutting down".to_string())?;

    let started = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    let outcome = shared
        .sessions
        .request(
            app,
            &grant.id,
            &grant.label,
            &host_id,
            &host_name,
            &command,
            timeout,
        )
        .await;

    let (result, via, session_id) = match outcome {
        SessionOutcome::Completed {
            session_id,
            exit,
            stdout,
            truncated,
        } => {
            let result = if exit.code.is_none() && exit.error_message.is_some() {
                Err(exit
                    .error_message
                    .unwrap_or_else(|| "The SSH command session failed".into()))
            } else {
                Ok(CommandRun {
                    exit_code: exit.code,
                    stdout,
                    stderr: String::new(),
                    truncated,
                })
            };
            (result, "tab", Some(session_id))
        }
        SessionOutcome::Failed(message) => (Err(message), "tab", None),
        SessionOutcome::Unavailable => (
            run_via_exec(&state.pool, &keystore_state, &host_id, &command, timeout).await,
            "exec",
            None,
        ),
        SessionOutcome::TimedOut(session_id) => (Err(timed_out(timeout)), "tab", session_id),
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    // Never the command body: an agent-supplied command may embed a password
    // in a heredoc or an env assignment, and the log redactor cannot see it.
    shared.record(ActivityEntry::command(CommandOutcome {
        grant_id: &grant.id,
        grant_label: &grant.label,
        host_id: &host_id,
        host_name: &host_name,
        input_bytes: command.len(),
        exit_code: result.as_ref().ok().and_then(|run| run.exit_code),
        duration_ms,
        error: result.as_ref().err().cloned(),
        session_id: session_id.clone(),
        via,
    }));

    let run = result?;
    Ok(json!({
        "exitCode": run.exit_code,
        "stdout": run.stdout,
        "stderr": run.stderr,
        "truncated": run.truncated,
        "durationMs": duration_ms,
        "sessionId": session_id,
        "interactive": via == "tab",
    }))
}

/// What a command produced, however it was run.
struct CommandRun {
    exit_code: Option<u32>,
    stdout: String,
    stderr: String,
    truncated: bool,
}

/// Run through an SSH exec channel so the command reaches the host unchanged.
async fn run_via_exec(
    pool: &sqlx::SqlitePool,
    keystore_state: &KeystoreState,
    host_id: &str,
    command: &str,
    timeout: Duration,
) -> std::result::Result<CommandRun, String> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut truncated = false;
    let marker = exec::OutputCap::new(MAX_COMMAND_OUTPUT_BYTES).marker();

    let exit_code = exec::run_command_on_host(
        pool,
        keystore_state,
        host_id,
        ExecRequest {
            command,
            timeout,
            max_output_bytes: MAX_COMMAND_OUTPUT_BYTES,
            messages: EXEC_MESSAGES,
        },
        None,
        |stream, bytes| {
            if bytes == marker.as_bytes() {
                truncated = true;
                return;
            }
            match stream {
                ExecStream::Stdout => stdout.extend_from_slice(bytes),
                ExecStream::Stderr => stderr.extend_from_slice(bytes),
            }
        },
    )
    .await
    .map_err(|error| error.to_string())?;

    Ok(CommandRun {
        exit_code,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        truncated,
    })
}

fn timed_out(timeout: Duration) -> String {
    format!(
        "Command timed out after {} seconds. It may still be running in its Luma tab; \
the user can interact with it there and its pane can be read with luma_pane_read.",
        timeout.as_secs()
    )
}

async fn pane_send(
    app: &AppHandle,
    shared: &Arc<McpShared>,
    grant: &McpGrant,
    arguments: &Value,
) -> ToolResult {
    let pane_id = required_str(arguments, "paneId")?;
    let text = required_str(arguments, "text")?;
    authorize_pane(shared, grant, &pane_id)?;

    if text.contains('\0') || text.len() > MAX_PANE_SEND_BYTES {
        return Err(format!(
            "text must be at most {MAX_PANE_SEND_BYTES} bytes and contain no null characters"
        ));
    }
    let submit = arguments
        .get("submit")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // A session is registered before authentication starts, and during that
    // window writes are consumed by the password prompt rather than a shell.
    // Refuse until a shell channel is actually open.
    let embedded = app.state::<EmbeddedSshManager>();
    if matches!(shared.taps.pane_kind(&pane_id), Some(PaneKind::Ssh { .. }))
        && !embedded.is_authenticated(&pane_id)
    {
        return Err(
            "that pane is still connecting. Wait for it to finish authenticating and retry.".into(),
        );
    }

    if grant.require_approval {
        let target = shared
            .taps
            .title(&pane_id)
            .unwrap_or_else(|| pane_id.clone());
        if !shared
            .approvals
            .request(app, &grant.id, &grant.label, "pane-send", &target, &text)
            .await
        {
            return Err("The user denied this input in Luma.".into());
        }
    }

    // Captured before the write so the caller reads exactly the output its own
    // input produced, rather than replaying what was already on screen.
    let cursor = shared.taps.cursor(&pane_id, &grant.id).unwrap_or_default();

    let payload = if submit {
        format!("{text}\r")
    } else {
        text.clone()
    };
    let pty = app.state::<PtyManager>();
    crate::commands::write_terminal_session(&pty, &embedded, &pane_id, payload)
        .map_err(|error| error.to_string())?;

    shared.record(ActivityEntry::pane_send(
        &grant.id,
        &grant.label,
        &pane_id,
        text.len(),
    ));

    Ok(json!({ "seq": cursor, "submitted": submit }))
}

async fn pane_read(shared: &Arc<McpShared>, grant: &McpGrant, arguments: &Value) -> ToolResult {
    let pane_id = required_str(arguments, "paneId")?;
    authorize_pane(shared, grant, &pane_id)?;

    let since = optional_u64(arguments, "sinceSeq", u64::MAX)?;
    let max_bytes = optional_u64(arguments, "maxBytes", MAX_PANE_READ_BYTES as u64)?
        .map(|value| value.max(1) as usize)
        .unwrap_or(DEFAULT_PANE_READ_BYTES);
    let wait = Duration::from_millis(
        optional_u64(arguments, "timeoutMs", MAX_PANE_READ_WAIT_MS)?.unwrap_or(0),
    );

    let read = shared
        .taps
        .read(&pane_id, &grant.id, since, max_bytes, wait)
        .await
        .ok_or_else(|| format!("pane {pane_id} is no longer shared with this grant"))?;

    Ok(json!({
        "text": read.text,
        "firstSeq": read.first_seq,
        "nextSeq": read.next_seq,
        "droppedBytes": read.dropped,
    }))
}
