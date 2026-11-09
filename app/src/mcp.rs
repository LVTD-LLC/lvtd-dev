//! The Model Context Protocol (MCP) server, mounted at `/mcp` when the
//! `mcp` feature is enabled. It lets coding agents (Claude Code, Cursor and
//! anything else speaking MCP's Streamable HTTP transport) call your product
//! as tools.
//!
//! The protocol and transport live in `app_core::mcp`: stateless
//! JSON-RPC over `POST`, version negotiation, error codes and the result
//! helpers. This module only describes the tools. Requests authenticate
//! with the same personal API key as the JSON API ([`ApiUser`]), so the
//! `mcp` feature depends on `api`; browser sessions are never accepted.
//!
//! One tool ships with every project, `get_account`, the MCP twin of
//! `GET /api/v1/me`. Add your product's tools in [`tools`] (the schema the
//! model sees) and [`call`] (what runs). Keep business logic in your own
//! modules and call in from here, as the API handlers do; report failures
//! the model can fix with [`tool_error`] so it can retry with better input.

use crate::SharedState;
use crate::api::{self, ApiError, ApiUser};
use app_auth::User;
use app_core::mcp::{Server, Tools, tool_error, tool_result};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::sync::LazyLock;

pub const PATH: &str = "/mcp";

/// What `initialize` tells a client. Built once; the title is the product name.
static SERVER: LazyLock<Server> = LazyLock::new(|| Server {
    name: env!("CARGO_PKG_NAME"),
    title: crate::views::SITE_NAME,
    version: env!("CARGO_PKG_VERSION"),
    instructions: Cow::Owned(format!(
        "{} exposes its product operations as MCP tools. Every tool runs as the account that owns the API key \
         this connection uses. Call get_account first to see which account that is.",
        crate::views::SITE_NAME
    )),
});

/// `POST /mcp`. The key is checked by the extractor before any JSON-RPC is read.
pub async fn handle(
    State(state): State<SharedState>,
    ApiUser(user): ApiUser,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    app_core::mcp::handle(&SERVER, &ProductTools { state: &state, user: &user }, &headers, &body).await
}

struct ProductTools<'a> {
    state: &'a SharedState,
    user: &'a User,
}

impl Tools for ProductTools<'_> {
    fn list(&self) -> Vec<Value> {
        tools()
    }

    async fn call(&self, name: &str, arguments: Value) -> anyhow::Result<Option<Value>> {
        Ok(Some(match call(self.state, self.user, name, arguments).await {
            Ok(Some(result)) => result,
            Ok(None) => return Ok(None),
            Err(ApiError::Status(status, message)) => tool_error(status, &message),
            Err(ApiError::Internal(error)) => return Err(error),
        }))
    }
}

/// The `tools/list` document: one entry per tool with the JSON Schema of
/// its arguments. Describe each tool for a model, not a programmer: say
/// what it returns and when to use it.
pub fn tools() -> Vec<Value> {
    vec![json!({
        "name": "get_account",
        "title": "Account",
        "description": "The account behind this API key: id, email, name, admin flag and join date. Call it first to confirm which account the tools act as.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false },
    })]
}

/// Run one tool. `Ok(None)` means the name is unknown; the protocol layer
/// turns that into an "unknown tool" error that lists the known names.
async fn call(state: &SharedState, user: &User, name: &str, arguments: Value) -> Result<Option<Value>, ApiError> {
    let _ = arguments; // get_account takes none; your tools read theirs here.
    Ok(Some(match name {
        "get_account" => {
            let account = api::account_json(state, user).await?;
            let who = match account["name"].as_str() {
                Some(name) if !name.is_empty() => format!("{name} <{}>", user.email),
                _ => user.email.clone(),
            };
            tool_result(format!("This key belongs to {who}."), account)
        }
        _ => return Ok(None),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_is_described_for_a_model() {
        let tools = tools();
        assert_eq!(tools[0]["name"], "get_account");
        for tool in &tools {
            assert!(tool["description"].as_str().unwrap().len() > 40, "{}", tool["name"]);
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert!(tool["annotations"]["readOnlyHint"].is_boolean(), "{}", tool["name"]);
        }
        assert!(SERVER.instructions.contains("get_account"));
        assert_eq!(SERVER.title, crate::views::SITE_NAME);
    }
}
