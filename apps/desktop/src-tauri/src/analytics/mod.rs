//! Opt-in product analytics: launch, duration, app/version/platform only.
//!
//! # Invariants
//!
//! Events carry a **persistent per-install identifier**, so repeated launches
//! from one install can be recognised as the same install. That is a real
//! privacy cost, and these rules are what keep it proportionate. Crossing any
//! of them means this must become opt-in:
//!
//! 1. The install id is **random, analytics-only, and created on opt-in**. It
//!    is deliberately NOT `device_state.device_id` — reusing that would link
//!    analytics to sync identity, so a sync peer would hold the same
//!    identifier the analytics server does. Opting out deletes it, so turning
//!    analytics off forgets the install rather than pausing it, and turning it
//!    back on starts a new identity.
//! 2. The id is **device-local and never synced**, so it cannot be used to
//!    join two of a user's devices together.
//! 3. Events carry no host, command, path or feature data. Error and panic
//!    diagnostics have their own separate consent in the diagnostics module.
//! 4. Nothing is collected until the user has answered the consent prompt. An
//!    absent `privacy.analytics` setting means undecided, which means off.
//!
//! Every entry point is infallible and non-blocking: no caller can be broken,
//! slowed or panicked by analytics being unavailable.

pub(crate) mod config;
mod sys;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rand::Rng;
use serde_json::{json, Value};

use config::Config;
use sys::SystemProps;

/// Device-local, never synced. See `storage::settings::DEVICE_LOCAL_SETTING_KEYS`.
pub const CONSENT_SETTING_KEY: &str = "privacy.analytics";
/// The persistent per-install identifier. Also device-local: syncing it would
/// let two of a user's devices be joined together, which is exactly what this
/// identifier must not enable.
pub const INSTALL_ID_SETTING_KEY: &str = "privacy.analyticsInstallId";

const EVENT_APP_LAUNCHED: &str = "app_launched";
const EVENT_APP_EXITED: &str = "app_exited";

/// Bounded so an offline device cannot grow the queue without limit. Upstream's
/// plugin has no cap.
const MAX_QUEUED: usize = 64;
/// The server's per-request batch limit.
const MAX_BATCH: usize = 25;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Bounds how long exit can wait on a flush.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(1500);
const MAX_BACKOFF_SLOTS: u32 = 8;

#[cfg(debug_assertions)]
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(not(debug_assertions))]
const FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// Managed state. A no-op when this build carries no configuration.
pub struct Analytics {
    inner: Option<Arc<Inner>>,
}

struct Inner {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    system: SystemProps,
    /// Set from the settings table on opt-in, cleared on opt-out.
    install_id: Mutex<Option<String>>,
    enabled: AtomicBool,
    launch_reported: AtomicBool,
    flusher_started: AtomicBool,
    started_at: Instant,
    session: Mutex<Option<String>>,
    queue: Mutex<VecDeque<Value>>,
    notify: tokio::sync::Notify,
    /// Flush intervals to skip after a failure.
    backoff_slots: AtomicU32,
}

/// Shared lifecycle analytics handle, initialized once during setup.
static GLOBAL: OnceLock<Analytics> = OnceLock::new();

/// Publishes the handle. Idempotent; later calls are ignored, which keeps a
/// second call in tests from replacing a live handle.
pub fn install(analytics: Analytics) {
    let _ = GLOBAL.set(analytics);
}

/// The installed lifecycle analytics handle, or an inert one before setup.
pub fn handle() -> &'static Analytics {
    static INERT: Analytics = Analytics { inner: None };
    GLOBAL.get().unwrap_or(&INERT)
}

/// Chains a panic reporter onto the existing hook.
///
/// Only a sanitized source basename/line is queued under diagnostics consent — a panic message can interpolate
/// arbitrary runtime values (a path, a host, the contents of a variable), so
/// it is never included. The previous hook still runs, so the existing log
/// output is unchanged.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(location) = info.location() {
            crate::diagnostics::report_panic(location.file(), location.line());
        }

        previous(info);
    }));
}

/// Builds the handle once, during setup. Does no I/O.
///
/// `consent` is the persisted `privacy.analytics` value; `None` means the user
/// has not been asked yet, in which case nothing is collected and nothing is
/// sent. `install_id` is the persisted identifier, which only exists once
/// consent has been given.
pub fn init(app_version: String, consent: Option<bool>, install_id: Option<String>) -> Analytics {
    let Some(config) = Config::from_build() else {
        tracing::info!("analytics not configured");
        return Analytics { inner: None };
    };

    let mut headers = reqwest::header::HeaderMap::new();
    let Ok(key) = reqwest::header::HeaderValue::from_str(&config.app_key) else {
        tracing::info!("analytics not configured");
        return Analytics { inner: None };
    };
    headers.insert("App-Key", key);
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    let Ok(client) = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .default_headers(headers)
        .build()
    else {
        tracing::info!("analytics not configured");
        return Analytics { inner: None };
    };

    let analytics = Analytics {
        inner: Some(Arc::new(Inner {
            client,
            endpoint: config.endpoint,
            system: SystemProps::new(app_version),
            install_id: Mutex::new(install_id),
            enabled: AtomicBool::new(false),
            launch_reported: AtomicBool::new(false),
            flusher_started: AtomicBool::new(false),
            started_at: Instant::now(),
            session: Mutex::new(None),
            queue: Mutex::new(VecDeque::new()),
            notify: tokio::sync::Notify::new(),
            backoff_slots: AtomicU32::new(0),
        })),
    };

    // Undecided (`None`) stays off: the consent prompt reports the launch when
    // the user accepts.
    if consent == Some(true) {
        analytics.set_enabled(true);
    }
    analytics
}

impl Analytics {
    /// Whether this build can send anything at all.
    pub fn is_configured(&self) -> bool {
        self.inner.is_some()
    }

    pub fn is_enabled(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| inner.enabled.load(Ordering::Relaxed))
    }

    /// Sets or clears the persistent install identifier. `None` on opt-out,
    /// after which events carry no identity until a new one is minted.
    pub fn set_install_id(&self, install_id: Option<String>) {
        if let Some(inner) = self.inner.as_ref() {
            *lock(&inner.install_id) = install_id;
        }
    }

    /// The current install identifier, for the Settings screen. It is the
    /// user's own identifier, so showing it is what lets them ask for the
    /// matching records to be deleted.
    pub fn install_id(&self) -> Option<String> {
        self.inner
            .as_ref()
            .and_then(|inner| lock(&inner.install_id).clone())
    }

    /// Runtime toggle, mirroring the Settings switch. Enabling starts a session
    /// and reports the launch once per process; disabling ends the session and
    /// drops anything queued.
    pub fn set_enabled(&self, enabled: bool) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        if inner.enabled.swap(enabled, Ordering::Relaxed) == enabled {
            return;
        }

        if enabled {
            *lock(&inner.session) = Some(new_session_id());
            inner.start_flusher();
            tracing::info!("analytics enabled");
            if !inner.launch_reported.swap(true, Ordering::Relaxed) {
                inner.track(EVENT_APP_LAUNCHED, None);
            }
        } else {
            *lock(&inner.session) = None;
            lock(&inner.queue).clear();
            tracing::info!("analytics disabled");
        }
    }

    /// Queues the exit event and flushes with a hard deadline. Returns promptly
    /// when disabled or when there is nothing to send.
    pub fn shutdown(&self) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        if !inner.enabled.load(Ordering::Relaxed) {
            return;
        }
        inner.track(
            EVENT_APP_EXITED,
            Some(json!({ "duration_seconds": inner.started_at.elapsed().as_secs() })),
        );
        if cfg!(test) {
            return;
        }
        // The main thread is not inside the tokio runtime here, so blocking on
        // the runtime handle is safe. Bounded so a hung network cannot hold the
        // app open.
        inner.flush_with_deadline();
    }
}

impl Inner {
    fn flush_with_deadline(&self) {
        let _ = tauri::async_runtime::block_on(async {
            tokio::time::timeout(SHUTDOWN_TIMEOUT, self.flush_once()).await
        });
    }

    /// Infallible and non-blocking. Drops the event when disabled.
    fn track(&self, name: &str, props: Option<Value>) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        let Some(session_id) = lock(&self.session).clone() else {
            return;
        };
        // Without an identifier there is nothing to attribute the event to, and
        // sending it anyway would produce a phantom install in the dashboard.
        let Some(install_id) = lock(&self.install_id).clone() else {
            return;
        };
        let event = json!({
            "timestamp": now_rfc3339(),
            "sessionId": session_id,
            "eventName": sys::clamp_event_name(name),
            "systemProps": self.system,
            "props": merge_install_id(props, &install_id),
        });

        let mut queue = lock(&self.queue);
        while queue.len() >= MAX_QUEUED {
            queue.pop_front();
        }
        queue.push_back(event);
        drop(queue);
        self.notify.notify_one();
    }

    /// Spawned on the first opt-in, so a user who declines never has a task.
    ///
    /// Never spawned under test: the suite exercises queueing and batching
    /// directly, and a background flusher would otherwise POST test events to
    /// the real ingest endpoint.
    fn start_flusher(self: &Arc<Self>) {
        if self.flusher_started.swap(true, Ordering::Relaxed) {
            return;
        }
        if cfg!(test) {
            return;
        }
        let inner = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(FLUSH_INTERVAL) => {}
                    _ = inner.notify.notified() => {}
                }
                if !inner.enabled.load(Ordering::Relaxed) {
                    continue;
                }
                let slots = inner.backoff_slots.load(Ordering::Relaxed);
                if slots > 0 {
                    inner.backoff_slots.store(slots - 1, Ordering::Relaxed);
                    continue;
                }
                inner.flush_once().await;
            }
        });
    }

    /// Drains the queue in batches. Never returns an error: a failed batch is
    /// requeued (transport/5xx) or dropped (4xx, which is permanent).
    async fn flush_once(&self) {
        loop {
            // The guard must not be held across the await: clippy's
            // `await_holding_lock` is denied in CI.
            let batch: Vec<Value> = {
                let mut queue = lock(&self.queue);
                let take = queue.len().min(MAX_BATCH);
                queue.drain(..take).collect()
            };
            if batch.is_empty() {
                return;
            }

            match self
                .client
                .post(self.endpoint.clone())
                .json(&batch)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    self.backoff_slots.store(0, Ordering::Relaxed);
                }
                Ok(response) if response.status().is_server_error() => {
                    self.requeue(batch);
                    self.back_off();
                    return;
                }
                Ok(response) => {
                    // 4xx is permanent (bad key, unknown app): dropping is the
                    // only way out of an otherwise endless retry.
                    tracing::debug!(
                        status = response.status().as_u16(),
                        "analytics batch rejected"
                    );
                    return;
                }
                Err(_) => {
                    // The error is discarded deliberately: its Display includes
                    // the endpoint URL. Same rule as sync::providers::network_error.
                    tracing::debug!("analytics endpoint unreachable");
                    self.requeue(batch);
                    self.back_off();
                    return;
                }
            }
        }
    }

    /// Puts a failed batch back at the front, preserving order.
    fn requeue(&self, batch: Vec<Value>) {
        let mut queue = lock(&self.queue);
        for event in batch.into_iter().rev() {
            if queue.len() >= MAX_QUEUED {
                queue.pop_back();
            }
            queue.push_front(event);
        }
    }

    fn back_off(&self) {
        let slots = self.backoff_slots.load(Ordering::Relaxed);
        self.backoff_slots
            .store((slots * 2).clamp(1, MAX_BACKOFF_SLOTS), Ordering::Relaxed);
    }
}

/// A poisoned lock must never take the app down over analytics.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

/// A fresh install identifier. Random and analytics-only: deliberately not
/// `device_state.device_id`, which sync also carries.
pub fn new_install_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Attaches the install id as a custom prop. Aptabase's `sessionId` is
/// process-scoped by design, so cross-launch grouping has to ride in `props`,
/// where only strings and numbers are permitted.
fn merge_install_id(props: Option<Value>, install_id: &str) -> Value {
    let mut object = match props {
        Some(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    object.insert("install_id".into(), Value::String(install_id.to_string()));
    Value::Object(object)
}

/// Matches Aptabase's own scheme so server-side validation passes: the server
/// divides by 100_000_000 and rejects ids more than 10 minutes in the future or
/// more than 7 days old.
fn new_session_id() -> String {
    let epoch_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let random: u64 = rand::thread_rng().gen_range(0..=99_999_999);
    (epoch_seconds * 100_000_000 + random).to_string()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_INSTALL_ID: &str = "11111111-1111-4111-8111-111111111111";

    fn configured() -> Analytics {
        // `init` reads the compile-time config, which the shipped defaults make
        // valid; `consent: None` leaves it off so each test opts in explicitly.
        init("0.14.2".into(), None, Some(TEST_INSTALL_ID.into()))
    }

    #[test]
    fn flush_with_deadline_runs_outside_runtime() {
        let analytics = configured();
        analytics.inner.as_ref().unwrap().flush_with_deadline();
    }

    #[test]
    fn an_unconfigured_handle_collects_nothing() {
        let analytics = Analytics { inner: None };
        assert!(!analytics.is_configured());
        assert!(!analytics.is_enabled());
        // Must not panic.
        analytics.set_enabled(true);
        analytics.set_install_id(Some("id".into()));
        assert!(!analytics.is_enabled());
        analytics.shutdown();
    }

    #[test]
    fn undecided_consent_stays_off() {
        let analytics = init("0.14.2".into(), None, None);
        assert!(!analytics.is_enabled());
        let inner = analytics.inner.as_ref().unwrap();
        assert!(lock(&inner.queue).is_empty());
        assert!(lock(&inner.session).is_none());
    }

    #[test]
    fn enabling_reports_the_launch_exactly_once() {
        let analytics = configured();
        analytics.set_enabled(true);
        analytics.set_enabled(false);
        analytics.set_enabled(true);

        let inner = analytics.inner.as_ref().unwrap();
        let queue = lock(&inner.queue);
        // The re-enable must not double-count the launch.
        assert_eq!(queue.len(), 0, "the disable cleared the queue");
        assert!(inner.launch_reported.load(Ordering::Relaxed));
    }

    #[test]
    fn the_launch_event_is_queued_on_opt_in() {
        let analytics = configured();
        analytics.set_enabled(true);
        let inner = analytics.inner.as_ref().unwrap();
        let queue = lock(&inner.queue);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0]["eventName"], EVENT_APP_LAUNCHED);
        assert_eq!(queue[0]["props"]["install_id"], TEST_INSTALL_ID);
    }

    #[test]
    fn every_event_carries_the_same_install_id() {
        // The whole point of the persistent id: two launches from one install
        // are recognisable as the same install.
        let analytics = configured();
        analytics.set_enabled(true);
        let inner = analytics.inner.as_ref().unwrap();
        inner.track("second_event", None);
        let queue = lock(&inner.queue);
        assert_eq!(queue.len(), 2);
        for event in queue.iter() {
            assert_eq!(event["props"]["install_id"], TEST_INSTALL_ID);
        }
    }

    #[test]
    fn nothing_is_collected_without_an_install_id() {
        // Opt-out clears the id; an event sent afterwards would be a phantom
        // install in the dashboard.
        let analytics = configured();
        analytics.set_enabled(true);
        let inner = analytics.inner.as_ref().unwrap();
        lock(&inner.queue).clear();
        analytics.set_install_id(None);

        inner.track("ignored", None);
        assert!(lock(&inner.queue).is_empty());
    }

    #[test]
    fn the_install_id_does_not_displace_an_events_own_props() {
        let merged = merge_install_id(Some(json!({ "duration_seconds": 12 })), TEST_INSTALL_ID);
        assert_eq!(merged["duration_seconds"], 12);
        assert_eq!(merged["install_id"], TEST_INSTALL_ID);
    }

    #[test]
    fn install_ids_are_unique_per_install() {
        assert_ne!(new_install_id(), new_install_id());
    }

    #[test]
    fn disabling_clears_the_session_and_the_queue() {
        let analytics = configured();
        analytics.set_enabled(true);
        analytics.set_enabled(false);

        let inner = analytics.inner.as_ref().unwrap();
        assert!(lock(&inner.session).is_none());
        assert!(lock(&inner.queue).is_empty());
        // Tracking after opt-out is a no-op.
        inner.track("ignored", None);
        assert!(lock(&inner.queue).is_empty());
    }

    #[test]
    fn the_queue_is_bounded_and_keeps_the_newest_events() {
        let analytics = configured();
        analytics.set_enabled(true);
        let inner = analytics.inner.as_ref().unwrap();
        for i in 0..(MAX_QUEUED * 2) {
            inner.track(&format!("event_{i}"), None);
        }
        let queue = lock(&inner.queue);
        assert_eq!(queue.len(), MAX_QUEUED);
        assert_eq!(
            queue[MAX_QUEUED - 1]["eventName"],
            format!("event_{}", MAX_QUEUED * 2 - 1)
        );
    }

    #[test]
    fn the_payload_matches_the_ingest_schema() {
        let analytics = configured();
        analytics.set_enabled(true);
        let inner = analytics.inner.as_ref().unwrap();
        let queue = lock(&inner.queue);
        let event = queue[0].as_object().unwrap();

        let mut keys: Vec<&str> = event.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "eventName",
                "props",
                "sessionId",
                "systemProps",
                "timestamp"
            ]
        );
        assert_eq!(event["systemProps"]["appVersion"], "0.14.2");
        assert!(event["systemProps"]["sdkVersion"]
            .as_str()
            .unwrap()
            .starts_with("luma-analytics@"));
        // The scope invariant: no locale, no osVersion, no engineVersion.
        let mut system_keys: Vec<&str> = event["systemProps"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        system_keys.sort_unstable();
        assert_eq!(
            system_keys,
            [
                "appVersion",
                "engineName",
                "isDebug",
                "osName",
                "sdkVersion"
            ]
        );
    }

    #[test]
    fn the_exit_event_carries_only_a_duration() {
        let analytics = configured();
        analytics.set_enabled(true);
        let inner = analytics.inner.as_ref().unwrap();
        inner.track(
            EVENT_APP_EXITED,
            Some(json!({ "duration_seconds": inner.started_at.elapsed().as_secs() })),
        );
        let queue = lock(&inner.queue);
        let event = &queue[queue.len() - 1];
        assert_eq!(event["eventName"], EVENT_APP_EXITED);
        let props = event["props"].as_object().unwrap();
        // A duration and the install id — nothing naming a host or a feature.
        assert_eq!(props.len(), 2);
        assert!(props["duration_seconds"].is_u64());
        assert_eq!(props["install_id"], TEST_INSTALL_ID);
    }

    #[test]
    fn session_ids_are_fresh_and_server_parseable() {
        let first: u64 = new_session_id().parse().unwrap();
        let second: u64 = new_session_id().parse().unwrap();
        assert_ne!(first, second, "each session must be distinct");

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // The server rejects ids >10 min in the future or >7 days old.
        assert!(first / 100_000_000 <= now + 60);
        assert!(first / 100_000_000 >= now - 60);
    }

    #[test]
    fn a_new_session_starts_on_each_opt_in() {
        let analytics = configured();
        analytics.set_enabled(true);
        let inner = analytics.inner.as_ref().unwrap();
        let first = lock(&inner.session).clone();
        analytics.set_enabled(false);
        analytics.set_enabled(true);
        let second = lock(&inner.session).clone();
        assert!(first.is_some() && second.is_some());
        assert_ne!(first, second);
    }

    #[test]
    fn requeue_preserves_order_and_the_cap() {
        let analytics = configured();
        analytics.set_enabled(true);
        let inner = analytics.inner.as_ref().unwrap();
        lock(&inner.queue).clear();

        inner.track("third", None);
        let batch = vec![
            json!({ "eventName": "first" }),
            json!({ "eventName": "second" }),
        ];
        inner.requeue(batch);

        let queue = lock(&inner.queue);
        let names: Vec<&str> = queue
            .iter()
            .map(|e| e["eventName"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["first", "second", "third"]);
    }

    #[test]
    fn backoff_grows_then_saturates() {
        let analytics = configured();
        let inner = analytics.inner.as_ref().unwrap();
        inner.back_off();
        assert_eq!(inner.backoff_slots.load(Ordering::Relaxed), 1);
        for _ in 0..10 {
            inner.back_off();
        }
        assert_eq!(
            inner.backoff_slots.load(Ordering::Relaxed),
            MAX_BACKOFF_SLOTS
        );
    }
}
