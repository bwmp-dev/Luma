//! Desktop opt-out diagnostics (mobile defaults off). Only fixed categories, source basenames and coarse startup timing.
//! No exception/panic messages, stacks, install IDs, connection data or disk queue.
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub const CONSENT_SETTING_KEY: &str = "privacy.diagnostics";
/// Resolve only after settings loaded successfully. Missing desktop preferences
/// (fresh installs and upgrades alike) enable diagnostics; malformed values fail
/// closed. Explicit choices always win, independently of usage analytics.
pub fn initial_enabled(saved: Option<&Value>) -> bool {
    resolve_enabled(saved, cfg!(any(target_os = "android", target_os = "ios")))
}
fn resolve_enabled(saved: Option<&Value>, mobile: bool) -> bool {
    match saved {
        None => !mobile,
        Some(value) => value.as_bool().unwrap_or(false),
    }
}
const CAPACITY: usize = 32;
const MAX_ATTEMPTS: u8 = 3;
const MAX_AGE: Duration = Duration::from_secs(600);
static GLOBAL: OnceLock<Option<Arc<Diagnostics>>> = OnceLock::new();

struct Pending {
    route: &'static str,
    body: Value,
    created: Instant,
    attempts: u8,
    retry_at: Instant,
}
struct State {
    enabled: bool,
    queue: VecDeque<Pending>,
    counts: HashMap<&'static str, u32>,
}
pub struct Diagnostics {
    client: reqwest::Client,
    base: reqwest::Url,
    state: Mutex<State>,
    // Opt-out waits for any in-flight request, then clears everything. No request
    // can start after a successful opt-out until explicitly enabled again.
    transport: tokio::sync::Mutex<()>,
}

pub fn handle() -> Option<&'static Arc<Diagnostics>> {
    GLOBAL.get().and_then(Option::as_ref)
}
pub fn install(consent: bool) {
    let configured = crate::analytics::config::Config::from_build()
        .and_then(|c| Diagnostics::new(c.endpoint, &c.app_key, consent).map(Arc::new));
    if GLOBAL.set(configured).is_err() {
        return;
    }
    if let Some(client) = handle() {
        let client = Arc::clone(client);
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                client.flush().await;
            }
        });
    }
}
impl Diagnostics {
    fn new(base: reqwest::Url, app_key: &str, enabled: bool) -> Option<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "App-Key",
            reqwest::header::HeaderValue::from_str(app_key).ok()?,
        );
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .ok()?;
        Some(Self {
            client,
            base,
            state: Mutex::new(State {
                enabled,
                queue: VecDeque::new(),
                counts: HashMap::new(),
            }),
            transport: tokio::sync::Mutex::new(()),
        })
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn enabled(&self) -> bool {
        self.state().enabled
    }
    pub async fn set_enabled(&self, enabled: bool) {
        if !enabled {
            let mut state = self.state();
            state.enabled = false;
            state.queue.clear();
            state.counts.clear();
        }
        let _sending = self.transport.lock().await;
        let mut state = self.state();
        state.enabled = enabled;
        if !enabled {
            state.queue.clear();
            state.counts.clear();
        }
    }
    fn enqueue(&self, route: &'static str, body: Value) {
        let mut state = self.state();
        if !state.enabled {
            return;
        }
        if state.queue.len() == CAPACITY {
            state.queue.pop_front();
        }
        state.queue.push_back(Pending {
            route,
            body,
            created: Instant::now(),
            attempts: 0,
            retry_at: Instant::now(),
        });
    }
    fn error(&self, category: &str) {
        let category = safe_category(category);
        {
            let mut state = self.state();
            if !state.enabled {
                return;
            }
            let count = state.counts.entry(category).or_default();
            *count = count.saturating_add(1);
            // At most eight reports/category/process, rather than flooding on a loop.
            if *count > 128 || !count.is_power_of_two() {
                return;
            }
        }
        self.enqueue("error", error_body(category, None));
    }
    async fn flush(&self) {
        let _sending = self.transport.lock().await;
        // No tight retry loop; transient failures wait until the next 5-second tick.
        for _ in 0..4 {
            let next = {
                let mut state = self.state();
                if !state.enabled {
                    return;
                }
                if state
                    .queue
                    .front()
                    .is_some_and(|p| p.retry_at > Instant::now())
                {
                    return;
                }
                state.queue.pop_front()
            };
            let Some(mut pending) = next else {
                return;
            };
            if pending.created.elapsed() > MAX_AGE {
                continue;
            }
            pending.attempts += 1;
            let mut endpoint = self.base.clone();
            // Preserve a configured reverse-proxy prefix; replace only /events.
            let path = endpoint.path().strip_suffix("events").unwrap_or("/api/v0/");
            endpoint.set_path(&format!("{path}{}", pending.route));
            let response = self.client.post(endpoint).json(&pending.body).send().await;
            let retry_after = response
                .as_ref()
                .ok()
                .and_then(|r| r.headers().get(reqwest::header::RETRY_AFTER))
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
                .min(600);
            let retry = match response {
                Ok(r) => r.status().as_u16() == 429 || r.status().is_server_error(),
                Err(_) => true,
            };
            if retry && pending.attempts < MAX_ATTEMPTS && pending.created.elapsed() <= MAX_AGE {
                pending.retry_at = Instant::now()
                    + Duration::from_secs(retry_after.max(5 * (1 << (pending.attempts - 1))));
                let mut state = self.state();
                if state.enabled && state.queue.len() < CAPACITY {
                    state.queue.push_front(pending);
                }
                return;
            }
        }
    }
}
pub fn report_error(category: &str) {
    if let Some(client) = handle() {
        client.error(category);
    }
}
pub fn report_panic(file: &str, line: u32) {
    if let Some(client) = handle() {
        client.enqueue("error", error_body("rust-panic", safe_location(file, line)));
    }
}
pub fn report_startup(started: Instant, timestamp: String) {
    if let Some(client) = handle() {
        if client.enabled() && rand::random::<u8>() < 26 {
            client.enqueue("transaction", startup_body(started.elapsed(), timestamp));
        }
    }
}
fn labels() -> Value {
    json!({
        "eventId": uuid::Uuid::new_v4().to_string(),
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "release": concat!("luma@", env!("CARGO_PKG_VERSION")),
        "environment": if cfg!(debug_assertions) { "development" } else { "production" },
        "platform": std::env::consts::OS,
        "isDebug": cfg!(debug_assertions),
    })
}
fn error_body(category: &'static str, location: Option<String>) -> Value {
    let mut body = labels();
    body["errorType"] = json!(category);
    // Fixed safe category, NEVER the runtime Display/error/panic payload.
    body["errorMessage"] = json!(category);
    body["kind"] = json!(if category == "rust-panic" {
        "unhandled"
    } else {
        "handled"
    });
    body["severity"] = json!("error"); // A Rust panic may be caught, so don't claim a native crash.
    if let Some(location) = location {
        body["stackTrace"] = json!(location);
    }
    body
}
fn startup_body(duration: Duration, timestamp: String) -> Value {
    let mut body = labels();
    body["timestamp"] = json!(timestamp);
    body["name"] = json!("app.setup");
    body["operation"] = json!("app.startup");
    body["durationMs"] = json!((duration.as_millis().min(86_400_000) / 10 * 10) as u64);
    body["status"] = json!("ok");
    body
}
fn safe_location(file: &str, line: u32) -> Option<String> {
    let basename = file.rsplit(['/', '\\']).next()?;
    if line == 0
        || basename.len() > 100
        || !basename.ends_with(".rs")
        || !basename
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return None;
    }
    Some(format!("{basename}:{line}"))
}
fn safe_category(category: &str) -> &'static str {
    match category {
        "database" => "database",
        "migration" => "migration",
        "io" => "io",
        "invalid-input" => "invalid-input",
        "pty" => "pty",
        "serial" => "serial",
        "update-unavailable" => "update-unavailable",
        "sftp-failed" => "sftp-failed",
        "key-unavailable" => "key-unavailable",
        "keystore-locked" => "keystore-locked",
        "sync-auth-failed" => "sync-auth-failed",
        "sync-conflict" => "sync-conflict",
        "sync-unavailable" => "sync-unavailable",
        "sync-passphrase-required" => "sync-passphrase-required",
        "auth-failed" => "auth-failed",
        "connection-lost" => "connection-lost",
        "dns-failed" => "dns-failed",
        "host-key-changed" => "host-key-changed",
        "host-key-file-invalid" => "host-key-file-invalid",
        "host-key-rejected" => "host-key-rejected",
        "host-key-scan-failed" => "host-key-scan-failed",
        "host-key-scan-required" => "host-key-scan-required",
        "host-key-scan-requires-auth" => "host-key-scan-requires-auth",
        "host-unreachable" => "host-unreachable",
        "key-passphrase-invalid" => "key-passphrase-invalid",
        "mosh-bootstrap-failed" => "mosh-bootstrap-failed",
        "mosh-client-missing" => "mosh-client-missing",
        "mosh-server-missing" => "mosh-server-missing",
        "ssh-error" => "ssh-error",
        "timeout" => "timeout",
        _ => "other-error",
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn client(enabled: bool) -> Diagnostics {
        Diagnostics::new(
            reqwest::Url::parse("http://127.0.0.1:9/api/v0/events").unwrap(),
            "A-SH-test",
            enabled,
        )
        .unwrap()
    }
    #[test]
    fn private_text_is_never_serialized() {
        for raw in [
            "ssh://alice@private.example",
            "token=secret",
            "cat /private/file",
            "terminal output",
        ] {
            let body = error_body(safe_category(raw), None);
            assert!(!body.to_string().contains(raw));
            assert_eq!(body["errorType"], "other-error");
        }
        assert_eq!(
            safe_location("C:\\Users\\alice\\source\\main.rs", 42).unwrap(),
            "main.rs:42"
        );
        assert_eq!(
            safe_location("/home/alice/project/main.rs", 42).unwrap(),
            "main.rs:42"
        );
        assert!(safe_location("token=secret.rs", 42).is_none());
        let body = error_body("rust-panic", safe_location("/private/project/main.rs", 42));
        assert_eq!(body["stackTrace"], "main.rs:42");
        assert!(body.get("sessionId").is_none());
        assert!(body.get("installId").is_none());
        assert_eq!(body["isDebug"], cfg!(debug_assertions));
    }
    #[tokio::test]
    async fn consent_queue_and_rate_bounds() {
        let client = client(false);
        client.error("io");
        assert!(client.state().queue.is_empty());
        client.set_enabled(true).await;
        for _ in 0..1000 {
            client.error("io");
        }
        assert_eq!(client.state().queue.len(), 8);
        for _ in 0..100 {
            client.enqueue("error", error_body("io", None));
        }
        assert_eq!(client.state().queue.len(), CAPACITY);
        client.set_enabled(false).await;
        assert!(client.state().queue.is_empty());
        client.error("io");
        assert!(client.state().queue.is_empty());
    }
    #[test]
    fn startup_is_coarse_and_bounded() {
        let body = startup_body(Duration::from_millis(129), "2026-01-01T00:00:00Z".into());
        assert_eq!(body["durationMs"], 120);
        assert_eq!(body["name"], "app.setup");
        assert!(body.get("spans").is_none());
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use std::io::{Read, Write};
    fn server(statuses: Vec<u16>) -> (reqwest::Url, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = reqwest::Url::parse(&format!(
            "http://{}/api/v0/events",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for status in statuses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 1024];
                loop {
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .unwrap()
                            .trim()
                            .parse()
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(bytes).unwrap());
                write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            }
            requests
        });
        (url, handle)
    }
    #[tokio::test]
    async fn actual_http_retries_identical_payload_without_private_data() {
        let (url, server) = server(vec![429, 202]);
        let client = Diagnostics::new(url, "A-SH-synthetic", true).unwrap();
        client.error("ssh://private-user@private-host");
        client.flush().await;
        assert_eq!(client.state().queue.len(), 1);
        {
            let mut state = client.state();
            assert!(state.queue[0].retry_at > Instant::now());
            state.queue[0].retry_at = Instant::now();
        }
        client.flush().await;
        assert!(client.state().queue.is_empty());
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("POST /api/v0/error "));
        assert!(requests[0]
            .to_lowercase()
            .contains("app-key: a-sh-synthetic"));
        let bodies: Vec<_> = requests
            .iter()
            .map(|s| s.split_once("\r\n\r\n").unwrap().1)
            .collect();
        assert_eq!(bodies[0], bodies[1]);
        assert!(!bodies[0].contains("private"));
    }
    #[tokio::test]
    async fn auth_rejections_are_dropped_and_transactions_use_own_route() {
        let (url, server) = server(vec![401, 202]);
        let client = Diagnostics::new(url, "A-SH-synthetic", true).unwrap();
        client.error("io");
        client.enqueue(
            "transaction",
            startup_body(Duration::from_millis(123), "2026-01-01T00:00:00Z".into()),
        );
        client.flush().await;
        assert!(client.state().queue.is_empty());
        let requests = server.join().unwrap();
        assert!(requests[1].starts_with("POST /api/v0/transaction "));
    }
    #[tokio::test]
    async fn retries_and_age_are_bounded() {
        let (url, server) = server(vec![503, 503, 503]);
        let client = Diagnostics::new(url, "A-SH-synthetic", true).unwrap();
        client.error("io");
        for _ in 0..3 {
            if let Some(pending) = client.state().queue.front_mut() {
                pending.retry_at = Instant::now();
            }
            client.flush().await;
        }
        assert!(client.state().queue.is_empty());
        assert_eq!(server.join().unwrap().len(), 3);
        client.error("timeout");
        client.state().queue[0].created = Instant::now() - MAX_AGE - Duration::from_secs(1);
        client.flush().await;
        assert!(client.state().queue.is_empty());
    }
}

#[cfg(test)]
mod consent_transport_tests {
    use super::*;
    use std::io::{Read, Write};

    #[tokio::test]
    async fn opt_out_waits_for_in_flight_and_prevents_later_sends() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = reqwest::Url::parse(&format!(
            "http://{}/api/v0/events",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let (received_tx, received_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut buffer = [0; 4096];
            assert!(stream.read(&mut buffer).unwrap() > 0);
            received_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            write!(
                stream,
                "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        let client = Arc::new(Diagnostics::new(endpoint, "A-SH-synthetic", true).unwrap());
        client.error("io");
        client.error("timeout");
        let sender = Arc::clone(&client);
        let sending = tokio::spawn(async move { sender.flush().await });
        received_rx.await.unwrap();
        let disabler = Arc::clone(&client);
        let disabling = tokio::spawn(async move { disabler.set_enabled(false).await });
        tokio::task::yield_now().await;
        assert!(!client.enabled());
        assert!(!disabling.is_finished());
        release_tx.send(()).unwrap();
        disabling.await.unwrap();
        sending.await.unwrap();
        client.error("database");
        client.flush().await;
        assert!(client.state().queue.is_empty());
        server.join().unwrap();
    }
}

#[cfg(test)]
mod default_tests {
    use super::*;

    #[test]
    fn fresh_and_upgrade_defaults_and_explicit_choices() {
        for mobile in [false, true] {
            // No preference is identical for a fresh install and an upgrade.
            assert_eq!(resolve_enabled(None, mobile), !mobile);
            assert!(!resolve_enabled(Some(&json!(false)), mobile));
            assert!(resolve_enabled(Some(&json!(true)), mobile));
            for invalid in [Value::Null, json!("true"), json!(1)] {
                assert!(!resolve_enabled(Some(&invalid), mobile));
            }
        }
        assert_eq!(
            initial_enabled(None),
            !cfg!(any(target_os = "android", target_os = "ios"))
        );
    }

    #[tokio::test]
    async fn saved_opt_out_survives_reload_without_changing_analytics() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let settings = crate::storage::settings::all(&pool).await.unwrap();
        assert!(resolve_enabled(settings.get(CONSENT_SETTING_KEY), false));
        assert!(!resolve_enabled(settings.get(CONSENT_SETTING_KEY), true));
        crate::storage::settings::set(&pool, crate::analytics::CONSENT_SETTING_KEY, &json!(true))
            .await
            .unwrap();
        let upgraded = crate::storage::settings::all(&pool).await.unwrap();
        assert!(resolve_enabled(upgraded.get(CONSENT_SETTING_KEY), false));
        crate::storage::settings::set(&pool, CONSENT_SETTING_KEY, &json!(false))
            .await
            .unwrap();
        let reloaded = crate::storage::settings::all(&pool).await.unwrap();
        assert!(!initial_enabled(reloaded.get(CONSENT_SETTING_KEY)));
        assert_eq!(
            reloaded.get(crate::analytics::CONSENT_SETTING_KEY),
            Some(&json!(true))
        );
        crate::storage::settings::set(&pool, CONSENT_SETTING_KEY, &json!(true))
            .await
            .unwrap();
        let reenabled = crate::storage::settings::all(&pool).await.unwrap();
        assert!(initial_enabled(reenabled.get(CONSENT_SETTING_KEY)));
    }
}
