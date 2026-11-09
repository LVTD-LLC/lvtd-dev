#![no_main]

use app_core::mcp::{self, Server, Tools};
use axum::body::to_bytes;
use axum::http::{HeaderMap, StatusCode};
use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};
use std::sync::LazyLock;

static RUNTIME: LazyLock<tokio::runtime::Runtime> =
    LazyLock::new(|| tokio::runtime::Builder::new_current_thread().build().unwrap());
static SERVER: LazyLock<Server> =
    LazyLock::new(|| Server { name: "fuzz", title: "Fuzz app", version: "0", instructions: "".into() });

struct Echo;
impl Tools for Echo {
    fn list(&self) -> Vec<Value> {
        vec![json!({"name": "echo", "inputSchema": {"type": "object"}})]
    }

    async fn call(&self, name: &str, arguments: Value) -> anyhow::Result<Option<Value>> {
        Ok((name == "echo").then(|| mcp::tool_result("echo", arguments)))
    }
}

fn check_envelope(value: &Value) {
    assert_eq!(value["jsonrpc"], "2.0");
    assert!(value.get("id").is_some());
    assert!(value.get("result").is_some() ^ value.get("error").is_some());
}

fuzz_target!(|data: &[u8]| {
    RUNTIME.block_on(async {
        let response = mcp::handle(&SERVER, &Echo, &HeaderMap::new(), data).await;
        let status = response.status();
        assert!(matches!(status, StatusCode::OK | StatusCode::ACCEPTED | StatusCode::BAD_REQUEST));
        assert_eq!(response.headers()["cache-control"], "private, no-store");
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        if status == StatusCode::ACCEPTED {
            assert!(body.is_empty());
        } else {
            let value: Value = serde_json::from_slice(&body).unwrap();
            if let Value::Array(values) = &value {
                assert!(!values.is_empty());
                values.iter().for_each(check_envelope);
            } else {
                check_envelope(&value);
            }
        }
    });
});
