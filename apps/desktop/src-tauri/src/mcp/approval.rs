//! Per-action approval prompts for grants that ask for them.
//!
//! A tool call registers a pending request, emits it to the frontend, and waits
//! on a oneshot. Every path that is not an explicit "allow" resolves to
//! **deny**: the timeout, a missing window, app shutdown, and a dropped
//! resolver all fail closed. The agent is never left hanging and nothing is
//! ever approved by default.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;

/// Event carrying a request to the frontend.
pub(crate) const APPROVAL_EVENT: &str = "mcp-approval-request";
/// Long enough for someone to look up from another window, short enough that a
/// forgotten prompt does not pin an agent forever.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
/// Prompts show what the agent wants to do; a long command is elided rather
/// than allowed to push the buttons off screen.
const MAX_PREVIEW_CHARS: usize = 400;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApprovalRequest {
    pub id: String,
    pub grant_id: String,
    pub grant_label: String,
    /// `"run-command"` or `"pane-send"`.
    pub action: &'static str,
    /// Host or pane the action targets, named as the user knows it.
    pub target: String,
    pub preview: String,
}

#[derive(Default)]
pub(crate) struct ApprovalRegistry {
    pending: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

impl ApprovalRegistry {
    /// Ask the user, returning whether the action may proceed.
    pub(crate) async fn request(
        &self,
        app: &AppHandle,
        grant_id: &str,
        grant_label: &str,
        action: &'static str,
        target: &str,
        preview: &str,
    ) -> bool {
        // With no window there is nobody to ask, and silently proceeding would
        // turn "require approval" into "approve everything".
        if app.webview_windows().is_empty() {
            tracing::warn!(
                grant_id,
                action,
                "MCP: denied because no window is open to approve it"
            );
            return false;
        }

        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), sender);

        let request = ApprovalRequest {
            id: id.clone(),
            grant_id: grant_id.to_string(),
            grant_label: grant_label.to_string(),
            action,
            target: target.to_string(),
            preview: truncate(preview, MAX_PREVIEW_CHARS),
        };
        if app.emit(APPROVAL_EVENT, request).is_err() {
            self.pending.lock().unwrap().remove(&id);
            return false;
        }

        let allowed = matches!(
            tokio::time::timeout(APPROVAL_TIMEOUT, receiver).await,
            Ok(Ok(true))
        );
        // Covers the timeout path; resolve() has already removed it otherwise.
        self.pending.lock().unwrap().remove(&id);
        allowed
    }

    /// Answer a pending prompt. Returns false if it already timed out.
    pub(crate) fn resolve(&self, id: &str, allowed: bool) -> bool {
        let Some(sender) = self.pending.lock().unwrap().remove(id) else {
            return false;
        };
        sender.send(allowed).is_ok()
    }

    /// Deny everything outstanding — used on shutdown and when the grant behind
    /// a prompt is revoked while it is still on screen.
    pub(crate) fn deny_all(&self) {
        for (_, sender) in self.pending.lock().unwrap().drain() {
            let _ = sender.send(false);
        }
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    match value.char_indices().nth(max_chars) {
        Some((index, _)) => format!("{}…", &value[..index]),
        None => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolving_an_unknown_request_is_a_no_op() {
        let registry = ApprovalRegistry::default();
        assert!(!registry.resolve("missing", true));
    }

    #[tokio::test]
    async fn a_resolved_request_reports_the_answer() {
        for answer in [true, false] {
            let registry = ApprovalRegistry::default();
            let (sender, receiver) = oneshot::channel();
            registry
                .pending
                .lock()
                .unwrap()
                .insert("request".into(), sender);

            assert!(registry.resolve("request", answer));
            assert_eq!(receiver.await.unwrap(), answer);
            // A second answer finds nothing pending.
            assert!(!registry.resolve("request", answer));
        }
    }

    #[tokio::test]
    async fn deny_all_fails_every_outstanding_prompt_closed() {
        let registry = ApprovalRegistry::default();
        let (sender, receiver) = oneshot::channel();
        registry
            .pending
            .lock()
            .unwrap()
            .insert("request".into(), sender);

        registry.deny_all();
        assert!(!receiver.await.unwrap());
        assert!(!registry.resolve("request", true));
    }

    #[tokio::test]
    async fn a_dropped_resolver_denies_rather_than_hanging() {
        let (sender, receiver) = oneshot::channel::<bool>();
        drop(sender);
        // This is the shape the request path awaits: a closed channel must read
        // as denied, never as approved.
        assert!(!matches!(
            tokio::time::timeout(Duration::from_secs(1), receiver).await,
            Ok(Ok(true))
        ));
    }

    #[test]
    fn previews_are_elided_on_a_character_boundary() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdef", 3), "abc…");
        // Multi-byte input must not be sliced mid-character.
        assert_eq!(truncate("héllo wörld", 4), "héll…");
    }
}
