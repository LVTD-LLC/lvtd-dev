//! Model Context Protocol (MCP) server plumbing: the JSON-RPC framing and
//! the Streamable HTTP transport in its stateless form, so an app only
//! describes its tools.
//!
//! Every JSON-RPC message is one `POST` answered with `application/json`.
//! There are no sessions to track and no server-sent event stream to hold
//! open, so any replica can answer any request and nothing is held between
//! them; `GET` can simply be a 405, as the specification allows for servers
//! that never push messages. Supported protocol revisions are
//! [`SUPPORTED_VERSIONS`]; `initialize` echoes a supported requested one
//! and otherwise answers with [`PROTOCOL_VERSION`]. The 2025-03-26 batch
//! form is accepted too.
//!
//! Methods: `initialize`, `ping`, `tools/list`, `tools/call`. Anything else
//! is `-32601`; the server advertises tools only. Failures a client can
//! correct belong inside the tool result ([`tool_error`]), as the protocol
//! asks, so the model can retry with better input; only malformed traffic
//! becomes a JSON-RPC error. An internal error is `-32603` and travels on
//! the response as `Arc<anyhow::Error>` for the app's observability layer,
//! exactly like `AppError`.
//!
//! Authentication is the app's concern: run the bearer extractor before
//! calling [`handle`], so an unauthenticated request never reaches here.
//! The surface is small enough that it is written out rather than pulling
//! in an SDK and its transport stack.

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::future::Future;
use std::sync::Arc;

/// The newest protocol revision this module speaks.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const SUPPORTED_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

/// What `initialize` tells a client about this server.
pub struct Server {
    /// Machine name, usually the package name.
    pub name: &'static str,
    /// Human name shown by clients.
    pub title: &'static str,
    pub version: &'static str,
    /// Shown to the model once per connection; the tool descriptions carry the details.
    pub instructions: Cow<'static, str>,
}

/// An app's tools. `list` is the `tools/list` document; `call` runs one by
/// name and returns `Ok(None)` for a name it does not know, `Ok(Some(_))`
/// with a [`tool_result`] or [`tool_error`], and `Err` only for failures the
/// client cannot fix (reported as `-32603`).
pub trait Tools {
    fn list(&self) -> Vec<Value>;
    fn call(&self, name: &str, arguments: Value) -> impl Future<Output = anyhow::Result<Option<Value>>> + Send;
}

/// A successful tool result: readable text for the model plus the same data
/// as `structuredContent` for clients that keep it.
pub fn tool_result(text: impl Into<String>, structured: Value) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }], "structuredContent": structured, "isError": false })
}

/// A failure the model can act on, reported inside the result as the
/// protocol asks, with the HTTP status an equivalent REST call would send.
pub fn tool_error(status: StatusCode, message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "structuredContent": { "error": message, "status": status.as_u16() },
        "isError": true,
    })
}

fn rpc_result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: &Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8"), (header::CACHE_CONTROL, "private, no-store")],
        body.to_string(),
    )
        .into_response()
}

/// Answer one `POST` carrying a JSON-RPC message or a batch. Notifications
/// get an empty 202; requests get their responses.
pub async fn handle<T: Tools + Sync>(server: &Server, tools: &T, headers: &HeaderMap, body: &[u8]) -> Response {
    if let Some(version) = headers.get("mcp-protocol-version").and_then(|value| value.to_str().ok())
        && !SUPPORTED_VERSIONS.contains(&version.trim())
    {
        let supported = SUPPORTED_VERSIONS.join(", ");
        let error = rpc_error(
            &Value::Null,
            INVALID_REQUEST,
            format!("Unsupported MCP-Protocol-Version {version:?}. This server speaks: {supported}."),
        );
        return json_response(StatusCode::BAD_REQUEST, &error);
    }
    let message: Value = match serde_json::from_slice(body) {
        Ok(message) => message,
        Err(error) => {
            let error = rpc_error(&Value::Null, PARSE_ERROR, format!("Request body must be JSON-RPC: {error}."));
            return json_response(StatusCode::BAD_REQUEST, &error);
        }
    };
    let mut internal: Option<anyhow::Error> = None;
    let responses: Vec<Value> = match message {
        Value::Array(batch) if batch.is_empty() => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &rpc_error(&Value::Null, INVALID_REQUEST, "An empty JSON-RPC batch has nothing to answer."),
            );
        }
        Value::Array(batch) => {
            let mut responses = Vec::with_capacity(batch.len());
            for message in batch {
                if let Some(response) = dispatch(server, tools, message, &mut internal).await {
                    responses.push(response);
                }
            }
            responses
        }
        message => dispatch(server, tools, message, &mut internal).await.into_iter().collect(),
    };
    let mut response = match responses.as_slice() {
        [] => (StatusCode::ACCEPTED, [(header::CACHE_CONTROL, "private, no-store")]).into_response(),
        [single] if !body.trim_ascii_start().starts_with(b"[") => json_response(StatusCode::OK, single),
        _ => json_response(StatusCode::OK, &Value::Array(responses)),
    };
    if let Some(error) = internal {
        response.extensions_mut().insert(Arc::new(error));
    }
    response
}

/// One JSON-RPC message in: a response out for requests, `None` for
/// notifications and for responses to requests this server never sends.
async fn dispatch<T: Tools + Sync>(
    server: &Server,
    tools: &T,
    message: Value,
    internal: &mut Option<anyhow::Error>,
) -> Option<Value> {
    let Value::Object(message) = message else {
        return Some(rpc_error(&Value::Null, INVALID_REQUEST, "Each JSON-RPC message must be an object."));
    };
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(rpc_error(&id, INVALID_REQUEST, "Expected \"jsonrpc\": \"2.0\"."));
    }
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        // A response to a server request. This server makes none, so there is nothing to match it to.
        return None;
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    if id.is_null() {
        // Notifications (`notifications/initialized`, `notifications/cancelled`, ...) need no reply.
        return None;
    }
    if !(id.is_string() || id.is_number()) {
        return Some(rpc_error(&Value::Null, INVALID_REQUEST, "A request id must be a string or a number."));
    }
    Some(match request(server, tools, method, params).await {
        Ok(result) => rpc_result(&id, result),
        Err(RpcError::Code(code, message)) => rpc_error(&id, code, message),
        Err(RpcError::Internal(error)) => {
            tracing::error!(error = ?error, method, "mcp request failed");
            if internal.is_none() {
                *internal = Some(error);
            }
            rpc_error(&id, INTERNAL_ERROR, "internal error")
        }
    })
}

enum RpcError {
    Code(i64, String),
    Internal(anyhow::Error),
}

async fn request<T: Tools + Sync>(server: &Server, tools: &T, method: &str, params: Value) -> Result<Value, RpcError> {
    match method {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(Value::as_str);
            let version = requested.filter(|v| SUPPORTED_VERSIONS.contains(v)).unwrap_or(PROTOCOL_VERSION);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": server.name, "title": server.title, "version": server.version },
                "instructions": server.instructions,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools.list() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            if !arguments.is_object() {
                return Err(RpcError::Code(INVALID_PARAMS, "Tool arguments must be a JSON object.".into()));
            }
            match tools.call(name, arguments).await {
                Ok(Some(result)) => Ok(result),
                Ok(None) => {
                    let known = tools
                        .list()
                        .iter()
                        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                        .join(", ");
                    Err(RpcError::Code(INVALID_PARAMS, format!("Unknown tool {name:?}. Tools: {known}.")))
                }
                Err(error) => Err(RpcError::Internal(error)),
            }
        }
        _ => Err(RpcError::Code(METHOD_NOT_FOUND, format!("Method not found: {method}."))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    struct Echo;

    impl Tools for Echo {
        fn list(&self) -> Vec<Value> {
            vec![json!({ "name": "echo", "description": "Echo", "inputSchema": { "type": "object" } })]
        }

        async fn call(&self, name: &str, arguments: Value) -> anyhow::Result<Option<Value>> {
            Ok(match name {
                "echo" => Some(tool_result(format!("echo {}", arguments["text"]), arguments)),
                "refuse" => Some(tool_error(StatusCode::UNPROCESSABLE_ENTITY, "No.")),
                "boom" => anyhow::bail!("synthetic failure"),
                _ => None,
            })
        }
    }

    const SERVER: Server =
        Server { name: "test", title: "Test", version: "0", instructions: Cow::Borrowed("Call echo.") };

    async fn post(headers: HeaderMap, body: &str) -> (StatusCode, bool, Value) {
        let response = handle(&SERVER, &Echo, &headers, body.as_bytes()).await;
        let status = response.status();
        let internal = response.extensions().get::<Arc<anyhow::Error>>().is_some();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
        (status, internal, value)
    }

    async fn rpc(method: &str, params: Value) -> (StatusCode, Value) {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string();
        let (status, internal, value) = post(HeaderMap::new(), &body).await;
        assert!(!internal);
        (status, value)
    }

    #[tokio::test]
    async fn initialize_negotiates_a_supported_version() {
        for (requested, negotiated) in [
            (Some("2025-06-18"), "2025-06-18"),
            (Some("2024-11-05"), "2024-11-05"),
            (Some("1999-01-01"), PROTOCOL_VERSION),
            (None, PROTOCOL_VERSION),
        ] {
            let params = match requested {
                Some(v) => json!({ "protocolVersion": v, "capabilities": {} }),
                None => json!({}),
            };
            let (status, response) = rpc("initialize", params).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(response["result"]["protocolVersion"], negotiated, "{requested:?}");
            assert_eq!(response["result"]["serverInfo"]["name"], "test");
            assert_eq!(response["result"]["instructions"], "Call echo.");
            assert_eq!(response["result"]["capabilities"]["tools"]["listChanged"], false);
        }
        let mut headers = HeaderMap::new();
        headers.insert("mcp-protocol-version", "1999-01-01".parse().unwrap());
        let (status, _, error) = post(headers, "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"]["code"], INVALID_REQUEST);
        assert!(error["error"]["message"].as_str().unwrap().contains(PROTOCOL_VERSION));
    }

    #[tokio::test]
    async fn tools_are_listed_called_and_errors_stay_in_results() {
        let (_, listed) = rpc("tools/list", json!({})).await;
        assert_eq!(listed["result"]["tools"][0]["name"], "echo");
        let (_, echoed) = rpc("tools/call", json!({ "name": "echo", "arguments": { "text": "hi" } })).await;
        assert_eq!(echoed["result"]["isError"], false);
        assert_eq!(echoed["result"]["content"][0]["text"], "echo \"hi\"");
        assert_eq!(echoed["result"]["structuredContent"]["text"], "hi");
        let (_, refused) = rpc("tools/call", json!({ "name": "refuse" })).await;
        assert_eq!(refused["result"]["isError"], true);
        assert_eq!(refused["result"]["structuredContent"]["status"], 422);
        assert_eq!(refused["result"]["content"][0]["text"], "No.");
        let (_, unknown) = rpc("tools/call", json!({ "name": "nope" })).await;
        assert_eq!(unknown["error"]["code"], INVALID_PARAMS);
        assert!(unknown["error"]["message"].as_str().unwrap().contains("echo"));
        let (_, array) = rpc("tools/call", json!({ "name": "echo", "arguments": [] })).await;
        assert_eq!(array["error"]["code"], INVALID_PARAMS);
        let (_, missing) = rpc("resources/list", json!({})).await;
        assert_eq!(missing["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(rpc("ping", json!({})).await.1["result"], json!({}));
    }

    #[tokio::test]
    async fn internal_failures_are_reported_and_kept_for_observability() {
        let body =
            json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": "boom" } }).to_string();
        let (status, internal, response) = post(HeaderMap::new(), &body).await;
        assert_eq!(status, StatusCode::OK);
        assert!(internal, "the source error rides on the response");
        assert_eq!(response["id"], 7);
        assert_eq!(response["error"]["code"], INTERNAL_ERROR);
        assert_eq!(response["error"]["message"], "internal error");
    }

    #[tokio::test]
    async fn notifications_batches_and_malformed_traffic() {
        let (status, _, body) =
            post(HeaderMap::new(), "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}").await;
        assert_eq!((status, body), (StatusCode::ACCEPTED, Value::Null));
        let (status, _, body) = post(HeaderMap::new(), "{\"jsonrpc\":\"2.0\",\"id\":5,\"result\":{}}").await;
        assert_eq!((status, body), (StatusCode::ACCEPTED, Value::Null));
        let batch = json!([
            { "jsonrpc": "2.0", "id": "a", "method": "ping" },
            { "jsonrpc": "2.0", "method": "notifications/cancelled" },
            { "jsonrpc": "2.0", "id": 2, "method": "tools/list" },
        ])
        .to_string();
        let (status, _, responses) = post(HeaderMap::new(), &batch).await;
        assert_eq!(status, StatusCode::OK);
        let responses = responses.as_array().unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!((responses[0]["id"].clone(), responses[1]["id"].clone()), (json!("a"), json!(2)));
        let (status, _, _) =
            post(HeaderMap::new(), "[{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}]").await;
        assert_eq!(status, StatusCode::ACCEPTED);
        for (body, status, code) in [
            ("nope", StatusCode::BAD_REQUEST, PARSE_ERROR),
            ("[]", StatusCode::BAD_REQUEST, INVALID_REQUEST),
            ("[42]", StatusCode::OK, INVALID_REQUEST),
            ("{\"jsonrpc\":\"1.0\",\"id\":1,\"method\":\"ping\"}", StatusCode::OK, INVALID_REQUEST),
            ("{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"ping\"}", StatusCode::OK, INVALID_REQUEST),
        ] {
            let (got, _, response) = post(HeaderMap::new(), body).await;
            assert_eq!(got, status, "{body}");
            let response = if let Some(batch) = response.as_array() { batch[0].clone() } else { response };
            assert_eq!(response["error"]["code"], code, "{body}: {response}");
        }
    }
}
