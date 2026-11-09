+++
title = "Setup API"
slug = "api"
description = "Give your users a way to interact with your platform through an API, with personal API keys and your own endpoints."
section = "developer-tools"
order = 10
+++

## What is included

With the `api` Cargo feature (on by default), every account can create personal API keys in **Settings**, and the app serves a JSON API at `/api/v1`:

| Method and path | Purpose |
| --- | --- |
| `GET /api/v1` | Service name, version, deployment revision and the docs link. No key required. |
| `GET /api/v1/me` | The account behind the key: id, email, name, admin flag and join date. |
| `GET /openapi.json` or `GET /api/v1/openapi.json` | The OpenAPI 3.1 document for every endpoint, generated from the code. No key required. |
| `GET /api/v1/docs` | The same document in Swagger UI. No key required. |

Unknown paths under `/api/v1` return a JSON 404. Every error is a JSON object with an `error` message and a matching status code: `401` for a missing, invalid or revoked key, `404` for unknown resources and `500` for unexpected failures.

## Create and use a key

Open **Settings**, find **API keys**, give the key a name and select **Create API key**. The full key is shown once, so copy it into your environment. The page keeps only a short prefix; Postgres stores a SHA-256 hash. Each account can hold ten keys, revoking takes effect on the next request, and deleting the account deletes its keys.

Send the key as a bearer token:

```
curl https://example.com/api/v1/me \
  -H "Authorization: Bearer $API_KEY"
```

Browser sessions are not accepted by the API, and the API sends no CORS headers, so neither cookies nor keys can be used from another website.

## Let applications connect with OAuth

Applications and hosted AI assistants that support OAuth 2.1 do not need a pasted key. The app is its own authorization server: a client registers itself at `/oauth/register`, sends your user to `/oauth/authorize`, where they sign in and approve the connection, and exchanges the resulting code at `/oauth/token` for an access token (`sro_...`) it sends exactly like a key, plus a refresh token it rotates on its own. Discovery documents live at `/.well-known/oauth-authorization-server` and `/.well-known/oauth-protected-resource`, and a `401` points at them, so clients find everything from the API URL alone. PKCE with `S256` is required of every client; access tokens last an hour and refresh tokens 30 days.

A token has the same access as the account that approved it, like a key. Users see every connection under **Connected apps** in **Settings** and can disconnect one at any time. This is also how agents connect to the [MCP server](/docs/mcp). `ApiUser` accepts both kinds of credential, so your endpoints need no changes.

## Reference documentation

The API describes itself. `/openapi.json` (also available at `/api/v1/openapi.json`) is an OpenAPI 3.1 document your users can feed to code generators, Postman, Insomnia or their coding agents. `/api/v1/docs` renders it in Swagger UI: select **Authorize**, paste a key, and try each endpoint from the browser. The key stays in that tab and is never saved. Swagger UI loads from jsDelivr, pinned to one version and checked with a Subresource Integrity hash (`app/src/views/api_docs.rs`).

The document is generated from your handlers with [utoipa](https://github.com/juhaku/utoipa) when the app starts, so it always matches what the server accepts and returns. There is no file to keep in sync.

## Add your own endpoints

`app/src/api.rs` owns the router. Register the handler in `documented_routes` with `routes!`, annotate it with `#[utoipa::path]`, take `ApiUser` as an argument to require a key, and return `Result<Response, ApiError>`. Describe bodies with structs that derive `ToSchema` and send them with `json_body`, so the document shows exactly what the handler sends:

```rust
fn documented_routes() -> OpenApiRouter<SharedState> {
    OpenApiRouter::new()
        .routes(routes!(info))
        .routes(routes!(me))
        .routes(routes!(list_widgets))
}

/// A widget you own.
#[derive(Serialize, ToSchema)]
struct Widget {
    id: Uuid,
    name: String,
}

#[derive(Serialize, ToSchema)]
struct WidgetList {
    widgets: Vec<Widget>,
}

/// Your widgets, newest first.
#[utoipa::path(
    get, path = "/widgets", tag = "widgets",
    security(("api_key" = [])),
    responses(
        (status = OK, description = "Your widgets", body = WidgetList),
        (status = UNAUTHORIZED, description = "Missing, invalid or revoked key", body = ErrorBody),
    ),
)]
async fn list_widgets(State(state): State<SharedState>, ApiUser(user): ApiUser) -> Result<Response, ApiError> {
    let widgets = crate::widgets::list_for_user(&state.pool, user.id).await?;
    Ok(json_body(StatusCode::OK, &WidgetList { widgets }))
}
```

The doc comment becomes the endpoint's summary, field doc comments become property descriptions, and `#[schema(example = ...)]` adds examples. Handlers that share a path go in the same `routes!(get_widget, delete_widget)`. A route added with plain `.route` still works but is missing from the document, so the unit test in `api.rs` lists every documented operation; add yours to it.

Keep SQL and business rules in your own modules, as the HTML handlers do, and scope every query by `user.id` so one account can never read another's data. Use `ApiError::status(StatusCode::UNPROCESSABLE_ENTITY, "...")` for validation failures; any other error is logged and becomes a JSON 500. Request bodies are limited to 64 KB and requests time out after 20 seconds, like the rest of the app.

## Turn it off

Remove `"api"` from the default features in `app/Cargo.toml`. The routes, the OpenAPI document, Swagger UI, the OAuth endpoints, the settings panels and the `utoipa` dependencies disappear; the `api_keys` and `oauth_*` tables stay in the schema so the feature can be enabled again without a migration.
