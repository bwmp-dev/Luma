//! JSON-RPC envelope and MCP method dispatch.
//!
//! Deliberately pure: this module parses bytes and builds values, so the whole
//! protocol surface is unit-testable without binding a socket. Anything needing
//! I/O is handed back to the caller as [`Dispatch::CallTool`].
//!
//! Two spec details that break real clients if missed:
//!
//! * A *notification* (no `id`) gets `202 Accepted` with an empty body, never a
//!   JSON-RPC response. Every client sends `notifications/initialized` right
//!   after `initialize`, and some error out if answered.
//! * No `Mcp-Session-Id` is issued. State lives in the grant row and the tap
//!   registry rather than in a session, so clients never send one back and the
//!   "session expired, restart" dance never applies.

use serde_json::{json, Value};

pub(crate) const SERVER_NAME: &str = "luma";
pub(crate) const LATEST_PROTOCOL_VERSION: &str = "2025-06-18";
/// Versions whose request/response shape this server matches. A client asking
/// for one of these gets it echoed back; anything else is answered with the
/// latest, which is what the spec asks for on an unknown version.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

pub(crate) const PARSE_ERROR: i64 = -32700;
pub(crate) const INVALID_REQUEST: i64 = -32600;
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const INVALID_PARAMS: i64 = -32602;

pub(crate) const TOOL_LIST_TARGETS: &str = "luma_list_targets";
pub(crate) const TOOL_RUN_COMMAND: &str = "luma_run_command";
pub(crate) const TOOL_PANE_SEND: &str = "luma_pane_send";
pub(crate) const TOOL_PANE_READ: &str = "luma_pane_read";

pub(crate) enum Dispatch {
    /// A complete JSON-RPC response body.
    Respond(Value),
    /// A notification: 202, no body.
    Accepted,
    /// Needs I/O; the caller runs it and formats the result.
    CallTool {
        id: Value,
        name: String,
        arguments: Value,
    },
}

/// Parse one request body and decide what to do with it.
pub(crate) fn dispatch(body: &[u8]) -> Dispatch {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Dispatch::Respond(error_response(Value::Null, PARSE_ERROR, "invalid JSON"));
    };
    // Batches were removed in the current spec and no client this server
    // targets sends them; rejecting is clearer than half-supporting them.
    if value.is_array() {
        return Dispatch::Respond(error_response(
            Value::Null,
            INVALID_REQUEST,
            "batch requests are not supported",
        ));
    }
    let Some(object) = value.as_object() else {
        return Dispatch::Respond(error_response(
            Value::Null,
            INVALID_REQUEST,
            "request must be a JSON object",
        ));
    };
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Dispatch::Respond(error_response(
            object.get("id").cloned().unwrap_or(Value::Null),
            INVALID_REQUEST,
            "missing method",
        ));
    };
    let params = object.get("params").cloned().unwrap_or(Value::Null);

    // No id means a notification: acknowledge, never answer.
    let Some(id) = object.get("id").filter(|id| !id.is_null()).cloned() else {
        return Dispatch::Accepted;
    };

    match method {
        "initialize" => Dispatch::Respond(result_response(id, initialize_result(&params))),
        "ping" => Dispatch::Respond(result_response(id, json!({}))),
        "tools/list" => {
            Dispatch::Respond(result_response(id, json!({ "tools": tool_definitions() })))
        }
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Dispatch::Respond(error_response(
                    id,
                    INVALID_PARAMS,
                    "tools/call requires a tool name",
                ));
            };
            if !is_known_tool(name) {
                return Dispatch::Respond(error_response(
                    id,
                    INVALID_PARAMS,
                    &format!("unknown tool: {name}"),
                ));
            }
            Dispatch::CallTool {
                id,
                name: name.to_string(),
                arguments: params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            }
        }
        // Declared capabilities are tools-only, so the resource and prompt
        // methods are genuinely absent rather than empty.
        other => Dispatch::Respond(error_response(
            id,
            METHOD_NOT_FOUND,
            &format!("unknown method: {other}"),
        )),
    }
}

fn is_known_tool(name: &str) -> bool {
    matches!(
        name,
        TOOL_LIST_TARGETS | TOOL_RUN_COMMAND | TOOL_PANE_SEND | TOOL_PANE_READ
    )
}

fn initialize_result(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = match requested {
        Some(version) if SUPPORTED_PROTOCOL_VERSIONS.contains(&version) => version,
        _ => LATEST_PROTOCOL_VERSION,
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": "Luma brokers SSH access on the user's behalf. Hosts and \
    panes reachable through these tools are the ones the user granted; credentials \
    stay inside Luma and are never exposed here. Call luma_list_targets first to see \
    what this grant can reach.",
    })
}

pub(crate) fn result_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub(crate) fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

/// A tool that ran and produced a value.
pub(crate) fn tool_result(id: Value, value: Value) -> Value {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    result_response(
        id,
        json!({
            "content": [{ "type": "text", "text": text }],
            "structuredContent": value,
            "isError": false,
        }),
    )
}

/// A tool that failed.
///
/// This is a *successful* JSON-RPC response carrying `isError`, not a protocol
/// error: the model can read and act on the former, and never sees the latter.
pub(crate) fn tool_failure(id: Value, message: &str) -> Value {
    result_response(
        id,
        json!({
            "content": [{ "type": "text", "text": message }],
            "isError": true,
        }),
    )
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": TOOL_LIST_TARGETS,
            "title": "List Luma targets",
            "description": "List the SSH hosts and live terminal panes this grant can reach. \
    Call this first: host and pane ids come from here, and anything not listed is not accessible.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        },
        {
            "name": TOOL_RUN_COMMAND,
            "title": "Run a command over SSH",
            "description": "Run one command on a granted host and return its output. Runs through \
    Luma's stored credentials, in a terminal tab the user can see and type into — so a command that \
    prompts for a sudo password will wait for them to answer it rather than failing. Commands run in \
    a persistent shell per host, so the working directory and environment carry over between calls. \
    Output is a terminal stream: stdout and stderr are merged into stdout, escape sequences are \
    stripped, and full-screen programs such as vim or top will not render. Luma defines a shell \
    function named __luma in the session and appends a call to it after each command to detect \
    completion. A command whose last line reads its own stdin (a heredoc, or a bare 'cat') cannot \
    be detected as finished and will run until the timeout.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "hostId": { "type": "string", "description": "Host id from luma_list_targets." },
                    "command": { "type": "string", "description": "Command line to run." },
                    "timeoutSeconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 600,
                        "description": "Wall-clock limit covering opening the session and running the \
    command. Defaults to 60. On timeout the command keeps running in its tab.",
                    },
                },
                "required": ["hostId", "command"],
                "additionalProperties": false,
            },
        },
        {
            "name": TOOL_PANE_SEND,
            "title": "Type into a shared pane",
            "description": "Send text to a terminal pane the user shared with this grant, as if \
    typed. Returns the read cursor from just before the write, so the output of what you sent can be \
    read with luma_pane_read starting at that cursor. Set submit to press Enter afterwards.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paneId": { "type": "string", "description": "Pane id from luma_list_targets." },
                    "text": { "type": "string", "description": "Text to type." },
                    "submit": {
                        "type": "boolean",
                        "description": "Append a carriage return, running the line. Defaults to false.",
                    },
                },
                "required": ["paneId", "text"],
                "additionalProperties": false,
            },
        },
        {
            "name": TOOL_PANE_READ,
            "title": "Read a shared pane",
            "description": "Read recent output from a shared pane, with terminal escape sequences \
    stripped. Pass the nextSeq from the previous read as sinceSeq to continue without gaps, and use \
    timeoutMs to wait for output instead of polling. A non-zero droppedBytes means output scrolled \
    out of the buffer before it was read. Note: this reads a byte stream, not the screen, so \
    line-oriented output (logs, build output, shell prompts) reads correctly but full-screen \
    applications such as vim, top or htop will not.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paneId": { "type": "string", "description": "Pane id from luma_list_targets." },
                    "sinceSeq": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Cursor from a previous read. Omit to read what is buffered.",
                    },
                    "timeoutMs": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 30000,
                        "description": "Wait this long for new output when already caught up. Defaults to 0.",
                    },
                    "maxBytes": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 262144,
                        "description": "Cap on returned text. Defaults to 65536.",
                    },
                },
                "required": ["paneId"],
                "additionalProperties": false,
            },
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn respond(body: &str) -> Value {
        match dispatch(body.as_bytes()) {
            Dispatch::Respond(value) => value,
            Dispatch::Accepted => panic!("expected a response, got an acknowledgement"),
            Dispatch::CallTool { .. } => panic!("expected a response, got a tool call"),
        }
    }

    fn error_code(body: &str) -> i64 {
        respond(body)["error"]["code"].as_i64().unwrap()
    }

    #[test]
    fn malformed_bodies_map_to_json_rpc_errors() {
        assert_eq!(error_code("not json"), PARSE_ERROR);
        assert_eq!(error_code("[]"), INVALID_REQUEST);
        assert_eq!(error_code("\"string\""), INVALID_REQUEST);
        assert_eq!(error_code(r#"{"jsonrpc":"2.0","id":1}"#), INVALID_REQUEST);
    }

    #[test]
    fn unknown_methods_are_method_not_found() {
        assert_eq!(
            error_code(r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#),
            METHOD_NOT_FOUND
        );
    }

    #[test]
    fn notifications_are_acknowledged_not_answered() {
        for body in [
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{}}"#,
            // An explicit null id is a notification too.
            r#"{"jsonrpc":"2.0","id":null,"method":"notifications/initialized"}"#,
        ] {
            assert!(
                matches!(dispatch(body.as_bytes()), Dispatch::Accepted),
                "{body}"
            );
        }
    }

    #[test]
    fn initialize_echoes_a_supported_version_and_falls_back_otherwise() {
        let supported = respond(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        );
        assert_eq!(supported["result"]["protocolVersion"], "2024-11-05");

        let unknown = respond(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
        );
        assert_eq!(
            unknown["result"]["protocolVersion"],
            LATEST_PROTOCOL_VERSION
        );

        let absent = respond(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);
        assert_eq!(absent["result"]["protocolVersion"], LATEST_PROTOCOL_VERSION);
        assert_eq!(absent["result"]["serverInfo"]["name"], SERVER_NAME);
        assert!(absent["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tools_list_advertises_exactly_the_four_namespaced_tools() {
        let listed = respond(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        let tools = listed["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec![
                TOOL_LIST_TARGETS,
                TOOL_RUN_COMMAND,
                TOOL_PANE_SEND,
                TOOL_PANE_READ
            ]
        );
        for tool in tools {
            assert!(tool["name"].as_str().unwrap().starts_with("luma_"));
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert!(!tool["description"].as_str().unwrap().is_empty());
        }
    }

    #[test]
    fn pane_read_warns_that_full_screen_apps_do_not_render() {
        let listed = respond(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        let tools = listed["result"]["tools"].as_array().unwrap();
        let pane_read = tools
            .iter()
            .find(|tool| tool["name"] == TOOL_PANE_READ)
            .unwrap();
        let description = pane_read["description"].as_str().unwrap();
        assert!(description.contains("full-screen"));
    }

    /// The description is the only place an agent learns that state carries over
    /// and that the two streams are merged. Getting this wrong silently breaks
    /// how a model reasons about its own commands.
    #[test]
    fn run_command_describes_the_interactive_session_it_actually_uses() {
        let listed = respond(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        let tools = listed["result"]["tools"].as_array().unwrap();
        let run = tools
            .iter()
            .find(|tool| tool["name"] == TOOL_RUN_COMMAND)
            .unwrap();
        let description = run["description"].as_str().unwrap();
        assert!(description.contains("sudo"));
        assert!(description.contains("carry over"));
        assert!(description.contains("merged"));
        // The old promise was the opposite of what this now does.
        assert!(!description.contains("no shell session between calls"));
    }

    #[test]
    fn tools_call_is_handed_back_for_execution() {
        let dispatched = dispatch(
            br#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"luma_run_command","arguments":{"hostId":"h","command":"uptime"}}}"#,
        );
        match dispatched {
            Dispatch::CallTool {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id, json!(7));
                assert_eq!(name, TOOL_RUN_COMMAND);
                assert_eq!(arguments["command"], "uptime");
            }
            _ => panic!("expected a tool call"),
        }
    }

    #[test]
    fn tools_call_rejects_missing_and_unknown_tools() {
        assert_eq!(
            error_code(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}}"#),
            INVALID_PARAMS
        );
        assert_eq!(
            error_code(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"rm_rf"}}"#
            ),
            INVALID_PARAMS
        );
    }

    #[test]
    fn tools_call_defaults_absent_arguments_to_an_empty_object() {
        let dispatched = dispatch(
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"luma_list_targets"}}"#,
        );
        match dispatched {
            Dispatch::CallTool { arguments, .. } => assert_eq!(arguments, json!({})),
            _ => panic!("expected a tool call"),
        }
    }

    #[test]
    fn tool_results_carry_text_and_structured_content() {
        let value = json!({ "exitCode": 0 });
        let response = tool_result(json!(1), value.clone());
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(response["result"]["structuredContent"], value);
        assert_eq!(response["result"]["content"][0]["type"], "text");
        assert!(response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("exitCode"));
    }

    #[test]
    fn tool_failures_are_readable_results_not_protocol_errors() {
        let response = tool_failure(json!(1), "vault is locked");
        assert!(response.get("error").is_none());
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(response["result"]["content"][0]["text"], "vault is locked");
    }
}
