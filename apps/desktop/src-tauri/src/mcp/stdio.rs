//! `luma --mcp-stdio`: bridges an MCP client's stdio transport to the running
//! app's loopback endpoint.
//!
//! This is the same executable rather than a bundled sidecar. A sidecar would
//! mean a second binary to build, sign, and notarize on every platform for
//! ~150 lines of "read a line, POST it, write the reply". `main` checks argv
//! before Tauri initialises, so nothing GUI — including the single-instance
//! plugin — ever runs in this process.
//!
//! The bridge, not the client, attaches the bearer token. That keeps the token
//! out of the client's own header handling and out of shell history.
//!
//! Absolute rule of the stdio transport: **stdout carries MCP messages and
//! nothing else.** Every diagnostic goes to stderr.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::path::PathBuf;
use std::time::Duration;

/// Long enough for a slow remote command, since the endpoint holds the request
/// open for the whole run.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(660);

pub const STDIO_FLAG: &str = "--mcp-stdio";

/// Entry point for the bridge. Returns the process exit code.
pub fn run() -> i32 {
    let token = match std::env::var("LUMA_MCP_TOKEN").ok().or_else(argv_token) {
        Some(token) if !token.trim().is_empty() => token,
        _ => {
            eprintln!(
                "luma {STDIO_FLAG}: no token. Set LUMA_MCP_TOKEN to a grant token from \
Luma's Settings > Agents."
            );
            return 2;
        }
    };

    let port = match read_endpoint_port() {
        Ok(port) => port,
        Err(message) => {
            eprintln!("luma {STDIO_FLAG}: {message}");
            return 3;
        }
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("luma {STDIO_FLAG}: could not start: {error}");
            return 4;
        }
    };
    runtime.block_on(pump(port, &token))
}

fn argv_token() -> Option<String> {
    // Accepted but undocumented: argv is world-readable via `ps` and Task
    // Manager, so the environment variable is the supported route.
    let mut args = std::env::args();
    while let Some(argument) = args.next() {
        if argument == "--token" {
            return args.next();
        }
        if let Some(value) = argument.strip_prefix("--token=") {
            return Some(value.to_string());
        }
    }
    None
}

fn endpoint_file_path() -> Option<PathBuf> {
    // Mirrors Tauri's app_data_dir for this bundle identifier. Resolved by hand
    // because no Tauri app exists in this process.
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
    });
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
        });

    Some(base?.join("dev.bwmp.luma").join(super::ENDPOINT_FILE))
}

fn read_endpoint_port() -> std::result::Result<u16, String> {
    let path =
        endpoint_file_path().ok_or_else(|| "could not locate Luma's data directory".to_string())?;
    let body = std::fs::read_to_string(&path)
        .map_err(|_| "Luma does not appear to be running. Start Luma, then retry.".to_string())?;
    let parsed: serde_json::Value = serde_json::from_str(&body)
        .map_err(|_| format!("{} is unreadable; restart Luma.", path.display()))?;
    parsed
        .get("port")
        .and_then(serde_json::Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .ok_or_else(|| format!("{} has no usable port; restart Luma.", path.display()))
}

async fn pump(port: u16, token: &str) -> i32 {
    let client = match reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() {
        Ok(client) => client,
        Err(error) => {
            eprintln!("luma {STDIO_FLAG}: could not start HTTP client: {error}");
            return 4;
        }
    };
    let endpoint = format!("http://127.0.0.1:{port}/mcp");

    let mut lines = BufReader::new(std::io::stdin()).lines();
    let mut stdout = std::io::stdout();
    loop {
        // Blocking reads on the current-thread runtime are fine: this process
        // does nothing but shuttle one message at a time.
        let Some(line) = lines.next() else {
            return 0; // stdin closed: the client is done with us.
        };
        let line = match line {
            Ok(line) => line,
            Err(error) if error.kind() == ErrorKind::BrokenPipe => return 0,
            Err(error) => {
                eprintln!("luma {STDIO_FLAG}: cannot read stdin: {error}");
                return 5;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let response = client
            .post(&endpoint)
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(line)
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                eprintln!("luma {STDIO_FLAG}: request failed: {error}");
                return 6;
            }
        };
        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if status == reqwest::StatusCode::ACCEPTED || body.trim().is_empty() {
            // A notification. Writing anything here would corrupt the stream.
            continue;
        }
        if !status.is_success() {
            eprintln!("luma {STDIO_FLAG}: endpoint returned {status}");
            // 401 means the grant is gone or the token is wrong; retrying every
            // message would just spam. Anything else may be transient.
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                return 7;
            }
            continue;
        }

        // One message per line, and the body must not contain a newline.
        let message = body.replace(['\n', '\r'], "");
        if let Err(error) = writeln!(stdout, "{message}").and_then(|()| stdout.flush()) {
            if error.kind() == ErrorKind::BrokenPipe {
                return 0;
            }
            // On Windows a GUI-subsystem process launched without redirected
            // handles has no usable stdout. Exit loudly rather than looking hung.
            eprintln!("luma {STDIO_FLAG}: cannot write stdout: {error}");
            return 5;
        }
    }
}
