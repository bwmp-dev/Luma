//! Loopback HTTP endpoint carrying MCP.
//!
//! The security gate is [`gate`] — a pure function over method, path and
//! headers — so every rejection rule is unit-testable without a socket. The
//! async half below only does I/O and hands bytes to [`super::protocol`].
//!
//! Rules worth stating explicitly, because getting any of them wrong is a
//! credential-boundary bug:
//!
//! * Bound to the literal `127.0.0.1`, never `"localhost"` — that resolves
//!   through the hosts file and could land somewhere else entirely.
//! * **Any** request carrying an `Origin` header is refused. Only browsers send
//!   one, no MCP client is a browser, and a DNS-rebinding attack from a page
//!   always sends one. Refusing outright closes the class in three lines.
//! * `Host` must name loopback and our own port, which catches rebinding
//!   vectors that do not set `Origin`.
//! * No `Access-Control-Allow-*` is ever emitted, so a browser that does reach
//!   the port cannot read the reply.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::header::{HeaderMap, AUTHORIZATION, CONTENT_TYPE, HOST, ORIGIN};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use tauri::AppHandle;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::errors::Result;
use crate::mcp::protocol::{self, Dispatch};
use crate::mcp::{grants, McpShared};

/// The single endpoint. Anything else is a bare 404 that does not echo back
/// what was asked for.
const MCP_PATH: &str = "/mcp";
/// Generous for JSON-RPC, small enough that a runaway client cannot buffer.
const MAX_BODY_BYTES: usize = 1024 * 1024;
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct RunningServer {
    pub port: u16,
    stop: watch::Sender<bool>,
}

impl RunningServer {
    pub(crate) fn stop(&self) {
        let _ = self.stop.send(true);
    }
}

/// Why a request was refused, before any grant lookup happened.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub status: StatusCode,
    /// Terse on purpose: a rejected caller learns that it was rejected, not
    /// which check it failed.
    pub message: &'static str,
}

impl Refusal {
    fn new(status: StatusCode, message: &'static str) -> Self {
        Self { status, message }
    }
}

/// Decide whether a request may proceed, returning the presented bearer token.
///
/// Pure: no I/O, no state. The token is *not* validated here — that is a
/// database lookup — only extracted.
pub(crate) fn gate<'h>(
    method: &Method,
    path: &str,
    headers: &'h HeaderMap,
    port: u16,
) -> std::result::Result<&'h str, Refusal> {
    // A browser reaching this port is always an attack; a legitimate client
    // never sets Origin.
    if headers.contains_key(ORIGIN) {
        return Err(Refusal::new(StatusCode::FORBIDDEN, "forbidden"));
    }
    if !host_is_loopback(headers, port) {
        return Err(Refusal::new(StatusCode::FORBIDDEN, "forbidden"));
    }
    if path != MCP_PATH {
        return Err(Refusal::new(StatusCode::NOT_FOUND, "not found"));
    }
    // GET would mean "open an SSE stream". Nothing here pushes notifications,
    // so the spec's 405 is the honest answer.
    if method != Method::POST {
        return Err(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed",
        ));
    }
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(grants::bearer_token)
        .ok_or_else(|| Refusal::new(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    Ok(token)
}

fn host_is_loopback(headers: &HeaderMap, port: u16) -> bool {
    let Some(host) = headers.get(HOST).and_then(|value| value.to_str().ok()) else {
        // HTTP/1.1 requires Host; its absence is not something to be lenient
        // about on a security boundary.
        return false;
    };
    let Some((name, host_port)) = host.rsplit_once(':') else {
        return false;
    };
    if host_port.parse::<u16>() != Ok(port) {
        return false;
    }
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

fn text_response(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(message.to_owned())))
        .expect("static response builds")
}

fn json_response(value: &serde_json::Value) -> Response<Full<Bytes>> {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .expect("json response builds")
}

/// Run one authenticated request body through grant lookup and MCP dispatch.
async fn dispatch_authenticated(
    app: AppHandle,
    shared: Arc<McpShared>,
    token: String,
    body: Bytes,
) -> Response<Full<Bytes>> {
    // Authenticate by digest: one indexed lookup, no iteration over grants.
    let grant = match super::authenticate(&app, &token).await {
        Ok(Some(grant)) => grant,
        Ok(None) => return text_response(StatusCode::UNAUTHORIZED, "unauthorized"),
        Err(error) => {
            tracing::warn!(error = %error, "MCP: grant lookup failed");
            return text_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    };

    match protocol::dispatch(&body) {
        Dispatch::Accepted => Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(Full::new(Bytes::new()))
            .expect("empty response builds"),
        Dispatch::Respond(value) => json_response(&value),
        Dispatch::CallTool {
            id,
            name,
            arguments,
        } => {
            let response = super::tools::call(&app, &shared, &grant, &name, arguments, id).await;
            json_response(&response)
        }
    }
}

/// Bind a loopback listener and serve until stopped.
pub(crate) async fn start(app: AppHandle, shared: Arc<McpShared>) -> Result<RunningServer> {
    serve(move |token, body| dispatch_authenticated(app.clone(), Arc::clone(&shared), token, body))
        .await
}

/// Bind, accept, and apply the security gate, delegating authenticated bodies
/// to `dispatch`.
///
/// Split from [`start`] so the gate, body cap, and status codes can be
/// exercised over a real socket without a Tauri app behind them.
async fn serve<D, F>(dispatch: D) -> Result<RunningServer>
where
    D: Fn(String, Bytes) -> F + Clone + Send + Sync + 'static,
    F: std::future::Future<Output = Response<Full<Bytes>>> + Send + 'static,
{
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await?;
    let port = listener.local_addr()?.port();
    let (stop, mut stop_rx) = watch::channel(false);

    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                biased;
                _ = stop_rx.changed() => break,
                accepted = listener.accept() => accepted,
            };
            let Ok((stream, _peer)) = accepted else {
                // A single failed accept (fd pressure, a client that vanished
                // between SYN and accept) must not take the listener down.
                continue;
            };
            let dispatch = dispatch.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                    let dispatch = dispatch.clone();
                    async move {
                        let (parts, body) = request.into_parts();
                        let token =
                            match gate(&parts.method, parts.uri.path(), &parts.headers, port) {
                                Ok(token) => token.to_string(),
                                Err(refusal) => {
                                    return Ok::<_, std::convert::Infallible>(text_response(
                                        refusal.status,
                                        refusal.message,
                                    ))
                                }
                            };
                        let Ok(collected) = Limited::new(body, MAX_BODY_BYTES).collect().await
                        else {
                            return Ok(text_response(
                                StatusCode::PAYLOAD_TOO_LARGE,
                                "request body too large",
                            ));
                        };
                        Ok(dispatch(token, collected.to_bytes()).await)
                    }
                });
                let _ = http1::Builder::new()
                    // hyper panics if a timeout is set without a timer, so the
                    // two must stay together.
                    .timer(TokioTimer::new())
                    .header_read_timeout(HEADER_READ_TIMEOUT)
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
        tracing::info!("MCP: listener stopped");
    });

    Ok(RunningServer { port, stop })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::HeaderValue;

    const PORT: u16 = 4242;

    fn header_map(pairs: &[(hyper::header::HeaderName, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(name.clone(), HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    fn authorized() -> HeaderMap {
        header_map(&[
            (HOST, "127.0.0.1:4242"),
            (AUTHORIZATION, "Bearer luma_mcp_token"),
        ])
    }

    fn gate_post(headers: &HeaderMap) -> std::result::Result<&str, Refusal> {
        gate(&Method::POST, MCP_PATH, headers, PORT)
    }

    #[test]
    fn a_well_formed_request_passes_and_yields_its_token() {
        assert_eq!(gate_post(&authorized()).unwrap(), "luma_mcp_token");
    }

    #[test]
    fn any_origin_header_is_refused() {
        let mut headers = authorized();
        headers.insert(ORIGIN, HeaderValue::from_static("https://evil.example"));
        assert_eq!(
            gate_post(&headers).unwrap_err().status,
            StatusCode::FORBIDDEN
        );

        // Even a same-origin-looking one: no legitimate client sends Origin.
        let mut headers = authorized();
        headers.insert(ORIGIN, HeaderValue::from_static("http://127.0.0.1:4242"));
        assert_eq!(
            gate_post(&headers).unwrap_err().status,
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn host_must_name_loopback_and_our_port() {
        for host in [
            "evil.example:4242",
            "127.0.0.1:9999",
            "127.0.0.1",
            "10.0.0.5:4242",
        ] {
            let headers = header_map(&[(HOST, host), (AUTHORIZATION, "Bearer t")]);
            assert_eq!(
                gate_post(&headers).unwrap_err().status,
                StatusCode::FORBIDDEN,
                "{host} should be refused"
            );
        }
        for host in ["127.0.0.1:4242", "localhost:4242", "[::1]:4242"] {
            let headers = header_map(&[(HOST, host), (AUTHORIZATION, "Bearer t")]);
            assert!(gate_post(&headers).is_ok(), "{host} should be accepted");
        }
    }

    #[test]
    fn a_missing_host_header_is_refused() {
        let headers = header_map(&[(AUTHORIZATION, "Bearer t")]);
        assert_eq!(
            gate_post(&headers).unwrap_err().status,
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn missing_or_malformed_credentials_are_unauthorized() {
        let headers = header_map(&[(HOST, "127.0.0.1:4242")]);
        assert_eq!(
            gate_post(&headers).unwrap_err().status,
            StatusCode::UNAUTHORIZED
        );

        for value in ["Basic abc", "luma_mcp_token", "Bearer "] {
            let headers = header_map(&[(HOST, "127.0.0.1:4242"), (AUTHORIZATION, value)]);
            assert_eq!(
                gate_post(&headers).unwrap_err().status,
                StatusCode::UNAUTHORIZED,
                "{value} should be unauthorized"
            );
        }
    }

    #[test]
    fn refusals_do_not_leak_which_check_failed() {
        let mut origin = authorized();
        origin.insert(ORIGIN, HeaderValue::from_static("https://evil.example"));
        let bad_host = header_map(&[(HOST, "evil.example:4242"), (AUTHORIZATION, "Bearer t")]);
        assert_eq!(
            gate_post(&origin).unwrap_err().message,
            gate_post(&bad_host).unwrap_err().message
        );
    }

    #[test]
    fn non_post_methods_are_method_not_allowed() {
        for method in [Method::GET, Method::DELETE, Method::PUT] {
            let refusal = gate(&method, MCP_PATH, &authorized(), PORT).unwrap_err();
            assert_eq!(
                refusal.status,
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} should be refused"
            );
        }
    }

    #[test]
    fn other_paths_are_a_bare_not_found() {
        let refusal = gate(&Method::POST, "/", &authorized(), PORT).unwrap_err();
        assert_eq!(refusal.status, StatusCode::NOT_FOUND);
        assert_eq!(refusal.message, "not found");

        // The path is never echoed back.
        let refusal = gate(&Method::POST, "/secret-probe", &authorized(), PORT).unwrap_err();
        assert!(!refusal.message.contains("secret-probe"));
    }

    #[test]
    fn path_checks_run_before_method_checks_for_unknown_paths() {
        let refusal = gate(&Method::GET, "/", &authorized(), PORT).unwrap_err();
        assert_eq!(refusal.status, StatusCode::NOT_FOUND);
    }

    /// Exercises the real hyper stack — bind, accept, header parsing, body
    /// limit, status codes — rather than the gate in isolation.
    #[tokio::test]
    async fn the_listener_enforces_the_gate_over_a_real_socket() {
        let running = serve(|token: String, body: Bytes| async move {
            json_response(&serde_json::json!({
                "token": token,
                "bodyLen": body.len(),
            }))
        })
        .await
        .unwrap();
        let endpoint = format!("http://127.0.0.1:{}/mcp", running.port);
        let client = reqwest::Client::new();

        // The port really is bound, on loopback.
        assert!(std::net::TcpListener::bind(("127.0.0.1", running.port)).is_err());

        let payload = "{\"jsonrpc\":\"2.0\"}";
        let ok = client
            .post(&endpoint)
            .bearer_auth("luma_mcp_token")
            .body(payload)
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), reqwest::StatusCode::OK);
        assert_eq!(
            ok.headers()
                .get(reqwest::header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap(),
            "application/json"
        );
        let body: serde_json::Value = ok.json().await.unwrap();
        assert_eq!(body["token"], "luma_mcp_token");
        assert_eq!(body["bodyLen"], payload.len());

        let unauthorized = client.post(&endpoint).body("{}").send().await.unwrap();
        assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

        let rebound = client
            .post(&endpoint)
            .bearer_auth("luma_mcp_token")
            .header(reqwest::header::ORIGIN, "https://evil.example")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(rebound.status(), reqwest::StatusCode::FORBIDDEN);
        // A browser must not be able to read the reply even if it reaches us.
        assert!(rebound
            .headers()
            .keys()
            .all(|name| !name.as_str().starts_with("access-control-")));

        let wrong_method = client
            .get(&endpoint)
            .bearer_auth("luma_mcp_token")
            .send()
            .await
            .unwrap();
        assert_eq!(
            wrong_method.status(),
            reqwest::StatusCode::METHOD_NOT_ALLOWED
        );

        let wrong_path = client
            .post(format!("http://127.0.0.1:{}/probe", running.port))
            .bearer_auth("luma_mcp_token")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(wrong_path.status(), reqwest::StatusCode::NOT_FOUND);
        assert!(!wrong_path.text().await.unwrap().contains("probe"));

        let oversized = client
            .post(&endpoint)
            .bearer_auth("luma_mcp_token")
            .body("x".repeat(MAX_BODY_BYTES + 1))
            .send()
            .await
            .unwrap();
        assert_eq!(oversized.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);

        // Stopping releases the port.
        running.stop();
        // One more connection lets the accept loop observe the stop signal.
        let _ = client.post(&endpoint).body("{}").send().await;
    }
}
