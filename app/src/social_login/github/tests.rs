use super::*;
use crate::social_login::Client as _;
use axum::{
    Json, Router,
    extract::{Form, Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post},
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

const CLIENT_ID: &str = "Ov23liTestClient0001";
const TOKEN: &str = "gho_test_access_token";

/// GitHub's token endpoint and REST API, served locally. `token`, `user` and
/// `emails` are the next responses; `last_form` and `revoked` record requests.
#[derive(Clone, Default)]
struct Responses {
    token: Arc<Mutex<Value>>,
    user: Arc<Mutex<Value>>,
    emails: Arc<Mutex<Value>>,
    last_form: Arc<Mutex<HashMap<String, String>>>,
    revoked: Arc<Mutex<Vec<String>>>,
}

pub(in crate::social_login) struct Mock {
    pub token: Arc<Mutex<Value>>,
    pub user: Arc<Mutex<Value>>,
    pub emails: Arc<Mutex<Value>>,
    pub last_form: Arc<Mutex<HashMap<String, String>>>,
    pub revoked: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn bearer(headers: &HeaderMap) -> bool {
    headers.get("authorization").is_some_and(|v| v == &format!("Bearer {TOKEN}")[..])
        && headers.get("user-agent").is_some()
}

pub(in crate::social_login) async fn fixture() -> (Client, Mock) {
    let r = Responses::default();
    *r.token.lock().await = json!({"access_token": TOKEN, "token_type": "bearer", "scope": "user:email"});
    *r.user.lock().await = json!({"id": 583231, "login": "octocat"});
    *r.emails.lock().await = json!([
        {"email": "octocat@users.noreply.github.com", "primary": false, "verified": true, "visibility": null},
        {"email": "Octocat@Example.com", "primary": true, "verified": true, "visibility": "public"},
    ]);
    let app = Router::new()
        .route(
            "/token",
            post(
                |State(m): State<Responses>, headers: HeaderMap, Form(form): Form<HashMap<String, String>>| async move {
                    assert_eq!(headers["accept"], "application/json");
                    *m.last_form.lock().await = form;
                    Json(m.token.lock().await.clone())
                },
            ),
        )
        .route(
            "/user",
            get(|State(m): State<Responses>, headers: HeaderMap| async move {
                if !bearer(&headers) {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                Json(m.user.lock().await.clone()).into_response()
            }),
        )
        .route(
            "/user/emails",
            get(|State(m): State<Responses>, headers: HeaderMap| async move {
                if !bearer(&headers) {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                Json(m.emails.lock().await.clone()).into_response()
            }),
        )
        .route(
            "/applications/{client}/token",
            delete(|State(m): State<Responses>, Path(client): Path<String>, Json(body): Json<Value>| async move {
                assert_eq!(client, CLIENT_ID);
                m.revoked.lock().await.push(body["access_token"].as_str().unwrap().to_owned());
                StatusCode::NO_CONTENT
            }),
        )
        .with_state(r.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut client = Client::new(
        Config { client_id: CLIENT_ID.into(), client_secret: "test-only-secret".into() },
        "http://localhost:3000",
    )
    .unwrap();
    client.token_url = format!("{origin}/token");
    client.api_url = origin;
    let Responses { token, user, emails, last_form, revoked } = r;
    (client, Mock { token, user, emails, last_form, revoked, task })
}

#[test]
fn configuration_fails_closed() {
    assert!(Config::parse("".into(), "".into()).unwrap().is_none());
    for (id, secret) in [(CLIENT_ID, ""), ("", "secret"), ("bad id", "secret"), ("a/b", "secret"), (CLIENT_ID, " ")] {
        assert!(Config::parse(id.into(), secret.into()).is_err(), "{id:?} {secret:?}");
    }
    assert!(Config::parse(CLIENT_ID.into(), "secret".into()).unwrap().is_some());
    for url in ["http://example.com", "https://example.com/path", "https://user@example.com"] {
        assert!(Client::new(Config::parse(CLIENT_ID.into(), "secret".into()).unwrap().unwrap(), url).is_err());
    }
}

#[test]
fn authorization_request_binds_state_and_pkce_without_exposing_secret() {
    let client = Client::new(
        Config { client_id: CLIENT_ID.into(), client_secret: "test-only-secret".into() },
        "https://app.example.com",
    )
    .unwrap();
    let url = reqwest::Url::parse(&client.authorization_url(
        "browser-state",
        "unused-nonce",
        "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
    ))
    .unwrap();
    assert_eq!((url.scheme(), url.host_str(), url.path()), ("https", Some("github.com"), "/login/oauth/authorize"));
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], CLIENT_ID);
    assert_eq!(query["redirect_uri"], "https://app.example.com/auth/github/callback");
    assert_eq!(query["scope"], "user:email");
    assert_eq!(query["state"], "browser-state");
    assert_eq!(query["code_challenge"], "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["prompt"], "select_account");
    assert_eq!(query.len(), 7);
    assert!(!url.as_str().contains("test-only-secret") && !url.as_str().contains("nonce"));
}

#[tokio::test]
async fn exchange_reads_numeric_id_and_primary_email_then_revokes_the_token() {
    let (client, mock) = fixture().await;
    let identity = client.identify("the-code", "the-verifier", "unused").await.unwrap();
    assert_eq!(identity.subject, "583231");
    assert_eq!(identity.signup_email.as_deref(), Some("octocat@example.com"));
    let form = mock.last_form.lock().await.clone();
    assert_eq!(form["code"], "the-code");
    assert_eq!(form["code_verifier"], "the-verifier");
    assert_eq!(form["redirect_uri"], "http://localhost:3000/auth/github/callback");
    assert_eq!(form["client_secret"], "test-only-secret");
    assert_eq!(*mock.revoked.lock().await, [TOKEN]);
}

#[tokio::test]
async fn provider_errors_reject_and_still_revoke_issued_tokens() {
    let (client, mock) = fixture().await;
    // GitHub reports a bad or reused code as a 200 response with an error field.
    *mock.token.lock().await =
        json!({"error": "bad_verification_code", "error_description": "The code passed is incorrect or expired."});
    assert!(client.identify("c", "v", "n").await.is_err());
    assert!(mock.revoked.lock().await.is_empty());
    *mock.token.lock().await = json!({"access_token": "gho_wrong"});
    assert!(client.identify("c", "v", "n").await.is_err(), "API rejects the token");
    assert_eq!(*mock.revoked.lock().await, ["gho_wrong"]);
    *mock.token.lock().await = json!({"access_token": TOKEN});
    *mock.user.lock().await = json!({"id": "not-a-number"});
    assert!(client.identify("c", "v", "n").await.is_err());
    *mock.user.lock().await = json!({"id": 1});
    *mock.emails.lock().await = json!({"message": "Not an array"});
    assert!(client.identify("c", "v", "n").await.is_err());
}

#[test]
fn only_a_verified_primary_deliverable_email_signs_up() {
    let emails = |list: Value| serde_json::from_value::<Vec<Email>>(list).unwrap();
    let ok = emails(json!([{"email": "Me@Example.com", "primary": true, "verified": true}]));
    assert_eq!(signup_email(&ok).as_deref(), Some("me@example.com"));
    for list in [
        json!([{"email": "me@example.com", "primary": true, "verified": false}]),
        json!([{"email": "me@example.com", "primary": false, "verified": true}]),
        json!([{"email": "1+me@users.noreply.github.com", "primary": true, "verified": true}]),
        json!([{"email": "a@@example.com", "primary": true, "verified": true}]),
        json!([{"email": "a b@example.com", "primary": true, "verified": true}]),
        json!([]),
    ] {
        assert!(signup_email(&emails(list.clone())).is_none(), "{list}");
    }
}

#[test]
fn configuration_from_environment() {
    const CHILD: &str = "APP_GITHUB_CONFIG_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let config = Config::from_env().unwrap().unwrap();
        assert_eq!(config.client_id, CLIENT_ID);
        assert_eq!(config.client_secret, "test-only-secret");
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "social_login::github::tests::configuration_from_environment"])
        .env(CHILD, "1")
        .env("GITHUB_CLIENT_ID", CLIENT_ID)
        .env("GITHUB_CLIENT_SECRET", "test-only-secret")
        .status()
        .unwrap();
    assert!(status.success());
}
