//! Automatic synchronization: the scheduler that decides when to call
//! [`super::sync_now`] without the user asking.
//!
//! One background task examines every configured vault on a short tick. The
//! tick is cheap on purpose — for each vault it reads a handful of aggregate
//! columns ([`super::local_change_stamp`]) and compares them with the stamp of
//! the last pushed bundle, so a quiet app does no network I/O and no bundle
//! assembly at all.
//!
//! Deciding here rather than in the frontend means the schedule keeps running
//! while the window is hidden, and that the "has anything changed?" question is
//! answered from the database that actually holds the answer instead of from
//! guesses about which React mutations imply a save.
//!
//! Three rules keep automation from becoming a nuisance:
//!
//! - A vault whose key is not already available is skipped. Automatic sync must
//!   never raise a passphrase prompt out of nowhere.
//! - A vault with unresolved conflicts is skipped. Resolving them is the user's
//!   decision and re-running would only re-report them.
//! - Failures back off exponentially, so an unreachable remote is retried
//!   occasionally rather than every tick.
//!
//! Every attempt is reported to the frontend over [`AUTO_SYNC_EVENT`] so the
//! title-bar indicator and the vault's sync panel show background activity the
//! same way they show a manual "Sync now".

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::collaboration::CollaborationRuntimeState;
use crate::keystore::KeystoreState;
use crate::sync::{AutoPushMode, AutoSyncCandidate, SyncReport, SyncRuntimeState};
use crate::AppState;

/// Window event carrying every automatic attempt. Payload: [`AutoSyncEvent`].
pub const AUTO_SYNC_EVENT: &str = "sync-auto";

/// How often the scheduler re-examines every vault. Short enough that "as soon
/// as I save" feels immediate once the debounce elapses, cheap enough to run
/// forever: the whole tick is a few indexed aggregates per vault.
const TICK: Duration = Duration::from_secs(5);

/// How long edits must settle before an `on-change` push. Renaming three hosts
/// in a row is one save, not three uploads.
const CHANGE_DEBOUNCE: Duration = Duration::from_secs(8);

/// Floor between two automatic attempts on the same vault, whatever asked for
/// them. Bounds how much traffic any combination of settings can produce.
const MIN_GAP: Duration = Duration::from_secs(45);

/// A foreground pull is skipped if the vault synced this recently. Alt-tabbing
/// repeatedly must not turn into a request per switch.
const FOCUS_COOLDOWN_SECS: i64 = 120;

/// First retry delay after a failure, doubled per consecutive failure.
const BACKOFF_BASE: Duration = Duration::from_secs(60);

/// Ceiling for the backoff, so a remote that comes back up is picked up within
/// the hour without the user touching anything.
const BACKOFF_MAX: Duration = Duration::from_secs(30 * 60);

/// Why the scheduler decided to sync. Purely descriptive: every reason runs the
/// same bidirectional sync, and the label only travels to the UI and the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    Startup,
    Focus,
    Change,
    PushInterval,
    PullInterval,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Focus => "focus",
            Self::Change => "change",
            Self::PushInterval => "push-interval",
            Self::PullInterval => "pull-interval",
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AutoSyncEvent {
    vault_id: String,
    reason: &'static str,
    /// `started`, `completed` or `failed`.
    phase: &'static str,
    report: Option<SyncReport>,
    error_category: Option<String>,
    error_message: Option<String>,
}

/// Per-vault scheduling bookkeeping. Deliberately not persisted: cadence
/// decisions that must survive a restart read `sync_state.last_synced_at` and
/// the pushed change stamp instead, both of which are already durable.
#[derive(Default)]
struct VaultTracker {
    /// When local changes were first observed, for the `on-change` debounce.
    dirty_since: Option<Instant>,
    /// Last attempt, successful or not — the input to [`MIN_GAP`].
    last_attempt: Option<Instant>,
    startup_done: bool,
    consecutive_failures: u32,
    retry_after: Option<Instant>,
}

#[derive(Default)]
struct Inner {
    vaults: HashMap<String, VaultTracker>,
    /// Set when the app returns to the foreground, consumed by the next tick.
    focus_pending: bool,
}

/// Scheduler state, managed by Tauri so the focus command can reach it.
#[derive(Default)]
pub struct AutoSyncState {
    inner: Mutex<Inner>,
}

impl AutoSyncState {
    /// Ask for a foreground pull. Recording a flag rather than syncing straight
    /// away keeps every eligibility rule in one place: the next tick applies the
    /// cooldown, the conflict check and the key check like any other reason.
    pub fn request_focus_sync(&self) {
        self.inner.lock().unwrap().focus_pending = true;
    }
}

/// Start the scheduler. Runs for the life of the app; a tick that fails is
/// logged and the loop continues, because the failure is almost always a
/// transient remote rather than a reason to stop scheduling.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(TICK).await;
            tick(&app).await;
        }
    });
}

async fn tick(app: &AppHandle) {
    let candidates = {
        let state = app.state::<AppState>();
        match super::auto_sync_candidates(&state.pool).await {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::warn!(%error, "auto sync: could not read the schedule");
                return;
            }
        }
    };

    let focus_pending = {
        let auto = app.state::<AutoSyncState>();
        let mut inner = auto.inner.lock().unwrap();
        // Vaults that disappeared (deleted, or sync disabled) must not keep
        // their backoff and debounce around for a later vault to inherit.
        inner
            .vaults
            .retain(|id, _| candidates.iter().any(|candidate| &candidate.vault_id == id));
        std::mem::take(&mut inner.focus_pending)
    };

    for candidate in candidates {
        if !candidate.settings.is_active() {
            continue;
        }
        match due(app, &candidate, focus_pending).await {
            Ok(Some(reason)) => run(app, &candidate.vault_id, reason).await,
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    vault_id = %candidate.vault_id,
                    %error,
                    "auto sync: could not evaluate the schedule",
                );
            }
        }
    }
}

/// The reason this vault should sync now, or `None` to leave it alone.
async fn due(
    app: &AppHandle,
    candidate: &AutoSyncCandidate,
    focus_pending: bool,
) -> crate::errors::Result<Option<Reason>> {
    let state = app.state::<AppState>();
    let runtime = app.state::<SyncRuntimeState>();
    let vault_id = candidate.vault_id.as_str();

    if super::has_pending_conflicts(&runtime, vault_id) {
        return Ok(None);
    }
    if !super::secret_available_unattended(&state.pool, &runtime, vault_id).await? {
        return Ok(None);
    }

    let now = Instant::now();
    let settings = &candidate.settings;
    // The cheapest checks that can rule the vault out come before the stamp
    // query, so a vault in backoff or inside its floor costs nothing.
    {
        let auto = app.state::<AutoSyncState>();
        let mut inner = auto.inner.lock().unwrap();
        let tracker = inner.vaults.entry(vault_id.to_string()).or_default();
        if tracker.retry_after.is_some_and(|retry| now < retry) {
            return Ok(None);
        }
        if tracker
            .last_attempt
            .is_some_and(|at| now.duration_since(at) < MIN_GAP)
        {
            return Ok(None);
        }
        if settings.pull_on_start && !tracker.startup_done {
            // Marked here rather than on success: a vault whose remote is down
            // at launch should fall back to its normal cadence, not retry the
            // startup pull forever.
            tracker.startup_done = true;
            return Ok(Some(Reason::Startup));
        }
    }

    // Seconds since the last *successful* sync. `None` means this vault has a
    // provider but has never completed one, which is itself a reason to try.
    let idle = candidate
        .last_synced_at
        .map(|at| Utc::now().timestamp().saturating_sub(at));

    if focus_pending
        && settings.pull_on_focus
        && idle.is_none_or(|seconds| seconds >= FOCUS_COOLDOWN_SECS)
    {
        return Ok(Some(Reason::Focus));
    }

    let dirty = if settings.push_mode == AutoPushMode::Off {
        false
    } else {
        candidate.has_local_changes(&state.pool).await?
    };

    {
        let auto = app.state::<AutoSyncState>();
        let mut inner = auto.inner.lock().unwrap();
        let tracker = inner.vaults.entry(vault_id.to_string()).or_default();
        match (dirty, tracker.dirty_since) {
            (true, None) => tracker.dirty_since = Some(now),
            (false, _) => tracker.dirty_since = None,
            (true, Some(_)) => {}
        }
        let settled = tracker
            .dirty_since
            .is_some_and(|since| now.duration_since(since) >= CHANGE_DEBOUNCE);
        if dirty {
            match settings.push_mode {
                AutoPushMode::Off => {}
                AutoPushMode::OnChange if settled => return Ok(Some(Reason::Change)),
                AutoPushMode::OnChange => {}
                AutoPushMode::Interval => {
                    if elapsed(idle, settings.push_interval_minutes) {
                        return Ok(Some(Reason::PushInterval));
                    }
                }
            }
        }
    }

    if settings.pull_interval_minutes != 0 && elapsed(idle, settings.pull_interval_minutes) {
        return Ok(Some(Reason::PullInterval));
    }
    Ok(None)
}

/// Whether `idle` seconds since the last sync has reached a cadence in minutes.
/// A vault that has never synced (`None`) is always due.
fn elapsed(idle: Option<i64>, minutes: u32) -> bool {
    idle.is_none_or(|seconds| seconds >= i64::from(minutes) * 60)
}

async fn run(app: &AppHandle, vault_id: &str, reason: Reason) {
    {
        let auto = app.state::<AutoSyncState>();
        let mut inner = auto.inner.lock().unwrap();
        let tracker = inner.vaults.entry(vault_id.to_string()).or_default();
        tracker.last_attempt = Some(Instant::now());
    }
    emit(
        app,
        AutoSyncEvent {
            vault_id: vault_id.to_string(),
            reason: reason.as_str(),
            phase: "started",
            report: None,
            error_category: None,
            error_message: None,
        },
    );

    let state = app.state::<AppState>();
    let result = super::sync_now(
        &state.pool,
        &app.state::<SyncRuntimeState>(),
        &app.state::<KeystoreState>(),
        &app.state::<CollaborationRuntimeState>(),
        &state.app_data_dir,
        vault_id,
    )
    .await;

    let auto = app.state::<AutoSyncState>();
    let event = match result {
        Ok(report) => {
            {
                let mut inner = auto.inner.lock().unwrap();
                let tracker = inner.vaults.entry(vault_id.to_string()).or_default();
                tracker.consecutive_failures = 0;
                tracker.retry_after = None;
                // A conflict leaves local changes unpushed, so the vault stays
                // dirty and the debounce clock restarts from the resolution.
                tracker.dirty_since = None;
            }
            tracing::debug!(
                vault_id,
                reason = reason.as_str(),
                pulled = report.pulled,
                pushed = report.pushed,
                conflicts = report.conflicts.len(),
                "auto sync finished",
            );
            AutoSyncEvent {
                vault_id: vault_id.to_string(),
                reason: reason.as_str(),
                phase: "completed",
                report: Some(report),
                error_category: None,
                error_message: None,
            }
        }
        Err(error) => {
            {
                let mut inner = auto.inner.lock().unwrap();
                let tracker = inner.vaults.entry(vault_id.to_string()).or_default();
                tracker.consecutive_failures = tracker.consecutive_failures.saturating_add(1);
                let backoff = BACKOFF_BASE
                    .saturating_mul(1u32 << tracker.consecutive_failures.min(5))
                    .min(BACKOFF_MAX);
                tracker.retry_after = Some(Instant::now() + backoff);
            }
            // Logged at debug: an automatic sync failing because the laptop is
            // offline is expected, and the user already sees it in the sync
            // panel through the event below.
            tracing::debug!(vault_id, %error, "auto sync failed");
            AutoSyncEvent {
                vault_id: vault_id.to_string(),
                reason: reason.as_str(),
                phase: "failed",
                report: None,
                error_category: Some(error.category().to_string()),
                error_message: Some(error.to_string()),
            }
        }
    };
    emit(app, event);
}

fn emit(app: &AppHandle, event: AutoSyncEvent) {
    if let Err(error) = app.emit(AUTO_SYNC_EVENT, event) {
        tracing::debug!(%error, "auto sync: could not report progress to the UI");
    }
}
