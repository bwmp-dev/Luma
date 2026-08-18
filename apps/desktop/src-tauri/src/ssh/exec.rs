//! Non-interactive command execution over SSH.
//!
//! One command, one exec channel, output captured under a shared byte cap. This
//! is deliberately separate from the interactive session path in `embedded.rs`:
//! there is no PTY here, so anything that assumes a terminal (a host's
//! `startup_command`, which `open_shell_channel` runs *instead of* the login
//! shell) must be cleared by the caller — see `ssh::non_interactive_config`.
//!
//! Callers differ only in what they do with the bytes: the snippet runner
//! streams them to the frontend as they arrive, the MCP server accumulates them
//! into a response. That is the `sink` parameter; everything else — the
//! channel-open timeout, the shared stdout/stderr cap, "transport disappeared"
//! detection, and the cancel/timeout race — is identical and lives here.

use std::time::Duration;

use russh::ChannelMsg;
use sqlx::{Row, SqlitePool};
use tokio::sync::watch;

use crate::errors::{LumaError, Result};
use crate::keystore::KeystoreState;
use crate::ssh::{self, AuthenticatedConnection};

/// Opening the exec channel is a round trip on an already-authenticated
/// connection; if it takes this long the transport is wedged.
const CHANNEL_OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Quick-connect hosts are not meant to be long-lived credentials.
const EPHEMERAL_MAX_AGE_SECS: i64 = 7 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecStream {
    Stdout,
    Stderr,
}

/// Error text for the two ways a run can be stopped from outside. Callers pass
/// their own so the message matches the user-facing operation.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExecMessages {
    /// Rendered as `{timed_out} after {n} seconds`.
    pub timed_out: &'static str,
    pub cancelled: &'static str,
    pub channel_open_timed_out: &'static str,
}

pub(crate) struct ExecRequest<'a> {
    pub command: &'a str,
    pub timeout: Duration,
    pub max_output_bytes: usize,
    pub messages: ExecMessages,
}

/// Reject host ids that no longer refer to something we should connect to.
/// Shared so every non-interactive caller inherits the ephemeral-expiry rule.
pub(crate) async fn validate_hosts(pool: &SqlitePool, host_ids: &[String]) -> Result<()> {
    for host_id in host_ids {
        let row = sqlx::query("SELECT is_ephemeral, created_at FROM hosts WHERE id = ?1")
            .bind(host_id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| LumaError::InvalidInput(format!("unknown hostId: {host_id}")))?;
        let is_ephemeral = row.get::<i64, _>("is_ephemeral") != 0;
        let created_at: i64 = row.get("created_at");
        if is_ephemeral
            && created_at
                < chrono::Utc::now()
                    .timestamp()
                    .saturating_sub(EPHEMERAL_MAX_AGE_SECS)
        {
            return Err(LumaError::InvalidInput(format!(
                "ephemeral host has expired: {host_id}"
            )));
        }
    }
    Ok(())
}

/// Resolve credentials, authenticate, run one command, and capture its output,
/// racing the whole thing against `request.timeout` and an optional `cancel`.
///
/// The timeout deliberately covers connection setup as well as execution: a
/// caller asking for "at most 60 seconds" means wall clock, not 60 seconds of
/// exec after an unbounded DNS and auth stall.
pub(crate) async fn run_command_on_host(
    pool: &SqlitePool,
    keystore_state: &KeystoreState,
    host_id: &str,
    request: ExecRequest<'_>,
    cancel: Option<watch::Receiver<bool>>,
    mut sink: impl FnMut(ExecStream, &[u8]),
) -> Result<Option<u32>> {
    let messages = request.messages;
    let timeout = request.timeout;

    let operation = async {
        let config = ssh::non_interactive_config(pool, keystore_state, host_id).await?;
        let handle = ssh::authenticated_handle(&config).await?;
        exec_capture(&handle, &request, &mut sink).await
    };

    // A `None` cancel still needs a receiver to select on. Keeping the sender
    // alive in scope means `changed()` never resolves, so the branch is inert.
    let (_cancel_keepalive, mut cancel) = match cancel {
        Some(rx) => (None, rx),
        None => {
            let (tx, rx) = watch::channel(false);
            (Some(tx), rx)
        }
    };

    tokio::pin!(operation);
    let timeout_sleep = tokio::time::sleep(timeout);
    tokio::pin!(timeout_sleep);
    tokio::select! {
        biased;
        changed = cancel.changed() => {
            if changed.is_ok() && *cancel.borrow() {
                Err(LumaError::SshConnection {
                    category: "connection-lost",
                    message: messages.cancelled.into(),
                })
            } else {
                operation.await
            }
        }
        result = &mut operation => result,
        _ = &mut timeout_sleep => Err(LumaError::SshConnection {
            category: "timeout",
            message: format!("{} after {} seconds", messages.timed_out, timeout.as_secs()),
        }),
    }
}

/// Run one command on an already-authenticated connection.
///
/// Returns the remote exit status, or `None` if the command finished without
/// the server sending one. A transport that vanishes mid-command is an error,
/// not a `None` exit code — otherwise a dropped connection is indistinguishable
/// from a command that simply did not report a status.
pub(crate) async fn exec_capture(
    handle: &AuthenticatedConnection,
    request: &ExecRequest<'_>,
    sink: &mut impl FnMut(ExecStream, &[u8]),
) -> Result<Option<u32>> {
    let mut channel = tokio::time::timeout(CHANNEL_OPEN_TIMEOUT, handle.channel_open_session())
        .await
        .map_err(|_| LumaError::SshConnection {
            category: "timeout",
            message: request.messages.channel_open_timed_out.into(),
        })?
        .map_err(ssh::embedded_connect_error)?;
    channel
        .exec(true, request.command.as_bytes())
        .await
        .map_err(ssh::embedded_connect_error)?;

    let mut cap = OutputCap::new(request.max_output_bytes);
    let mut exit_code = None;
    let mut disappeared = false;
    loop {
        let Some(message) = channel.wait().await else {
            disappeared = true;
            break;
        };
        match message {
            ChannelMsg::Data { data } => emit_capped(sink, ExecStream::Stdout, &data, &mut cap),
            ChannelMsg::ExtendedData { data, .. } => {
                emit_capped(sink, ExecStream::Stderr, &data, &mut cap)
            }
            ChannelMsg::ExitStatus { exit_status } => exit_code = Some(exit_status),
            ChannelMsg::Eof | ChannelMsg::Close => break,
            _ => {}
        }
    }
    if handle.is_closed() && exit_code.is_none() {
        disappeared = true;
    }
    if disappeared {
        return Err(LumaError::SshConnection {
            category: "connection-lost",
            message: "The SSH transport closed before the remote command finished".into(),
        });
    }
    Ok(exit_code)
}

fn emit_capped(
    sink: &mut impl FnMut(ExecStream, &[u8]),
    stream: ExecStream,
    data: &[u8],
    cap: &mut OutputCap,
) {
    let accepted = cap.accept(data);
    if !accepted.bytes.is_empty() {
        sink(stream, accepted.bytes);
    }
    if accepted.emit_marker {
        sink(stream, cap.marker().as_bytes());
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AcceptedOutput<'a> {
    pub bytes: &'a [u8],
    pub emit_marker: bool,
}

/// Byte budget shared across stdout and stderr, so a command that floods one
/// stream cannot starve the other of the truncation notice.
#[derive(Debug)]
pub(crate) struct OutputCap {
    limit: usize,
    remaining: usize,
    marker_emitted: bool,
}

impl OutputCap {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            remaining: limit,
            marker_emitted: false,
        }
    }

    pub(crate) fn marker(&self) -> String {
        format!("\n[output truncated after {} bytes]\n", self.limit)
    }

    pub(crate) fn accept<'a>(&mut self, data: &'a [u8]) -> AcceptedOutput<'a> {
        let accepted = data.len().min(self.remaining);
        self.remaining -= accepted;
        let truncated = accepted < data.len();
        let emit_marker = truncated && !self.marker_emitted;
        self.marker_emitted |= truncated;
        AcceptedOutput {
            bytes: &data[..accepted],
            emit_marker,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_cap_truncates_once_across_streams() {
        let mut cap = OutputCap::new(5);
        assert_eq!(
            cap.accept(b"abc"),
            AcceptedOutput {
                bytes: b"abc",
                emit_marker: false,
            }
        );
        assert_eq!(
            cap.accept(b"def"),
            AcceptedOutput {
                bytes: b"de",
                emit_marker: true,
            }
        );
        assert_eq!(
            cap.accept(b"more"),
            AcceptedOutput {
                bytes: b"",
                emit_marker: false,
            }
        );
    }

    #[test]
    fn cap_is_shared_across_streams_and_marks_once() {
        let mut cap = OutputCap::new(4);
        let mut emitted: Vec<(ExecStream, String)> = Vec::new();
        let mut sink = |stream: ExecStream, bytes: &[u8]| {
            emitted.push((stream, String::from_utf8_lossy(bytes).into_owned()));
        };

        emit_capped(&mut sink, ExecStream::Stdout, b"ab", &mut cap);
        // stderr eats the rest of the shared budget and trips the marker.
        emit_capped(&mut sink, ExecStream::Stderr, b"cdef", &mut cap);
        // Everything after the cap is dropped without a second marker.
        emit_capped(&mut sink, ExecStream::Stdout, b"ghi", &mut cap);

        assert_eq!(
            emitted,
            vec![
                (ExecStream::Stdout, "ab".to_string()),
                (ExecStream::Stderr, "cd".to_string()),
                (
                    ExecStream::Stderr,
                    "\n[output truncated after 4 bytes]\n".to_string()
                ),
            ]
        );
    }

    #[test]
    fn marker_reports_the_configured_limit() {
        assert_eq!(
            OutputCap::new(1024 * 1024).marker(),
            "\n[output truncated after 1048576 bytes]\n"
        );
    }
}
