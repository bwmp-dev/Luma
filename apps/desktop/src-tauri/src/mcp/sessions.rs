//! Asking the webview to open an interactive session for an agent.
//!
//! Terminal sessions are created frontend-first — the store builds the tab, the
//! terminal is attached and sized, and only then does it `invoke` the spawn — so
//! the backend cannot open one on its own. It asks, and waits for the session id
//! the webview reports back.
//!
//! Mirrors [`super::approval`] in both shape and discipline: every path that is
//! not an explicit "here is your session" resolves to a reason the agent can act
//! on, and nothing is left hanging. A missing window is not an error here but a
//! signal to fall back to a one-off exec connection — the tool still works with
//! no UI open, it just is not visible.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;

/// Event carrying a request to the frontend.
pub(crate) const SESSION_EVENT: &str = "mcp-session-request";

/// Opening a session can involve a host-key prompt and a credential prompt, so
/// this is generous. The caller additionally clamps it to whatever is left of
/// the agent's own timeout, so a short `timeoutSeconds` still wins.
const SESSION_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionRequest {
    pub id: String,
    pub grant_id: String,
    pub grant_label: String,
    pub host_id: String,
    /// Host as the user knows it, for the tab title.
    pub host_name: String,
}

/// What came back from the webview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionOutcome {
    /// A connected session the agent's command may run in.
    Ready(String),
    /// The webview tried and could not — host key refused, auth failed, the
    /// user closed the tab. Carries a message worth showing the agent.
    Failed(String),
    /// Nobody was there to ask: no window, shutdown, or the request timed out.
    /// The caller falls back to a one-off exec connection.
    Unavailable,
}

#[derive(Default)]
pub(crate) struct SessionRegistry {
    pending: Mutex<HashMap<String, oneshot::Sender<Result<String, String>>>>,
}

impl SessionRegistry {
    /// Ask the webview for a session on `host_id`, waiting up to `budget`.
    pub(crate) async fn request(
        &self,
        app: &AppHandle,
        grant_id: &str,
        grant_label: &str,
        host_id: &str,
        host_name: &str,
        budget: Duration,
    ) -> SessionOutcome {
        // No window means no store, no tab, and nobody to type a password into.
        // Not a failure — the caller runs the command the old way instead.
        if app.webview_windows().is_empty() {
            return SessionOutcome::Unavailable;
        }

        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), sender);

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

        let outcome = match tokio::time::timeout(budget.min(SESSION_TIMEOUT), receiver).await {
            Ok(Ok(Ok(session_id))) => SessionOutcome::Ready(session_id),
            Ok(Ok(Err(message))) => SessionOutcome::Failed(message),
            // Dropped resolver (window closed mid-request) or timed out.
            Ok(Err(_)) | Err(_) => SessionOutcome::Unavailable,
        };
        // Covers the timeout path; resolve() has already removed it otherwise.
        self.pending.lock().unwrap().remove(&id);
        outcome
    }

    /// Answer a pending request. Returns false if it already timed out.
    pub(crate) fn resolve(&self, id: &str, outcome: Result<String, String>) -> bool {
        let Some(sender) = self.pending.lock().unwrap().remove(id) else {
            return false;
        };
        sender.send(outcome).is_ok()
    }

    /// Abandon everything outstanding — shutdown, and when the grant behind a
    /// request is revoked while the webview is still opening its tab.
    pub(crate) fn cancel_all(&self) {
        self.pending.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolving_an_unknown_request_is_a_no_op() {
        let registry = SessionRegistry::default();
        assert!(!registry.resolve("missing", Ok("session".into())));
    }

    #[tokio::test]
    async fn a_resolved_request_reports_the_session() {
        let registry = SessionRegistry::default();
        let (sender, receiver) = oneshot::channel();
        registry
            .pending
            .lock()
            .unwrap()
            .insert("request".into(), sender);

        assert!(registry.resolve("request", Ok("session-1".into())));
        assert_eq!(receiver.await.unwrap(), Ok("session-1".into()));
        // A second answer finds nothing pending.
        assert!(!registry.resolve("request", Ok("session-1".into())));
    }

    #[tokio::test]
    async fn a_failure_from_the_webview_is_carried_through() {
        let registry = SessionRegistry::default();
        let (sender, receiver) = oneshot::channel();
        registry
            .pending
            .lock()
            .unwrap()
            .insert("request".into(), sender);

        assert!(registry.resolve("request", Err("host key refused".into())));
        assert_eq!(receiver.await.unwrap(), Err("host key refused".into()));
    }

    /// The whole point of the registry: a request nobody answers must not leave
    /// the agent's call hanging.
    #[tokio::test]
    async fn a_dropped_resolver_reads_as_unavailable() {
        let (sender, receiver) = oneshot::channel::<Result<String, String>>();
        drop(sender);
        let outcome = match tokio::time::timeout(Duration::from_secs(1), receiver).await {
            Ok(Ok(Ok(session_id))) => SessionOutcome::Ready(session_id),
            Ok(Ok(Err(message))) => SessionOutcome::Failed(message),
            Ok(Err(_)) | Err(_) => SessionOutcome::Unavailable,
        };
        assert_eq!(outcome, SessionOutcome::Unavailable);
    }

    #[tokio::test]
    async fn cancel_all_abandons_outstanding_requests() {
        let registry = SessionRegistry::default();
        let (sender, receiver) = oneshot::channel();
        registry
            .pending
            .lock()
            .unwrap()
            .insert("request".into(), sender);

        registry.cancel_all();
        // The sender is dropped, so the waiter resolves rather than hanging.
        assert!(receiver.await.is_err());
        assert!(!registry.resolve("request", Ok("session".into())));
    }
}
