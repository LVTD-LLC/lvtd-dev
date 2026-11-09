use super::*;
use crate::routes;
use axum::{Router, body::Body, http::Request};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[test]
fn flow_cookie_ignores_other_cookies_and_rejects_malformed_values() {
    let token = "a".repeat(64);
    let mut headers = HeaderMap::new();
    headers.append(header::COOKIE, HeaderValue::from_str(&format!("session={}", "b".repeat(64))).unwrap());
    headers.append(header::COOKIE, HeaderValue::from_str(&format!("other=123; social_login_flow={token}")).unwrap());
    assert_eq!(browser_token(&headers), Some(token.as_str()));
    for value in ["a".repeat(63), "a".repeat(65), "z".repeat(64), String::new()] {
        headers.insert(header::COOKIE, HeaderValue::from_str(&format!("social_login_flow={value}")).unwrap());
        assert_eq!(browser_token(&headers), None);
    }
    headers.insert(header::COOKIE, HeaderValue::from_static("social_login_flow; another=abc"));
    assert_eq!(browser_token(&headers), None);
}

/// The router with the providers `configure` adds, each pointed at a local double.
fn router(pool: PgPool, configure: impl FnOnce(&mut Providers)) -> Router {
    let cfg = app_core::Config {
        bind_addr: "127.0.0.1:0".into(),
        site_url: "http://localhost:3000".into(),
        database_url: String::new(),
        session_ttl_days: 30,
    };
    let (mut state, router) = crate::build(cfg, pool);
    drop(router);
    configure(&mut std::sync::Arc::get_mut(&mut state).unwrap().social_login);
    routes::router(state)
}
/// The router with only Google configured.
#[cfg(feature = "google")]
async fn app(pool: PgPool) -> (Router, google::tests::Mock) {
    let (client, mock) = google::tests::fixture().await;
    (router(pool, |providers| providers.insert("google", client)), mock)
}
/// The router with only GitHub configured.
#[cfg(feature = "github")]
async fn github_app(pool: PgPool) -> (Router, github::tests::Mock) {
    let (client, mock) = github::tests::fixture().await;
    (router(pool, |providers| providers.insert("github", client)), mock)
}
/// The router with Google and GitHub configured.
#[cfg(all(feature = "google", feature = "github"))]
async fn app_with_github(pool: PgPool) -> (Router, google::tests::Mock, github::tests::Mock) {
    let (google_client, google_mock) = google::tests::fixture().await;
    let (github_client, github_mock) = github::tests::fixture().await;
    let app = router(pool, |providers| {
        providers.insert("google", google_client);
        providers.insert("github", github_client);
    });
    (app, google_mock, github_mock)
}

#[cfg(feature = "github")]
/// Like `start_flow`, for GitHub: no nonce, and any code works with the mock.
async fn start_github_flow(app: &Router, path: &str, form: &str, session: &str) -> (String, String) {
    let (status, headers, _) = send(app, path, Some(form), session).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let url = reqwest::Url::parse(headers[header::LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(url.host_str(), Some("github.com"));
    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(params["scope"], "user:email");
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    let flow = cookie(&headers, FLOW_COOKIE);
    assert!(headers[header::SET_COOKIE].to_str().unwrap().contains("Path=/auth/github/callback"));
    (format!("/auth/github/callback?state={}&code=github-code", params["state"]), format!("{flow}; {session}"))
}
async fn send(app: &Router, path: &str, body: Option<&str>, cookie: &str) -> (StatusCode, HeaderMap, String) {
    let mut req = if body.is_some() {
        Request::post(path)
            .header(header::ORIGIN, "http://localhost:3000")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
    } else {
        Request::get(path)
    };
    req = req.header(header::COOKIE, cookie);
    let response = app.clone().oneshot(req.body(Body::from(body.unwrap_or("").to_owned())).unwrap()).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}
fn cookie(headers: &HeaderMap, name: &str) -> String {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|s| s.starts_with(&format!("{name}=")))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .into()
}
#[cfg(feature = "google")]
async fn start_flow(app: &Router, path: &str, form: &str, session: &str) -> (String, String) {
    let (status, headers, _) = send(app, path, Some(form), session).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let url = reqwest::Url::parse(headers[header::LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(url.host_str(), Some("accounts.google.com"));
    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(params["scope"], "openid email");
    assert_eq!(params["code_challenge_method"], "S256");
    assert!(!params.contains_key("client_secret"));
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    let path = format!("/auth/google/callback?state={}&code={}", params["state"], params["nonce"]);
    (path, format!("{}; {session}", cookie(&headers, FLOW_COOKIE)))
}
#[cfg(feature = "google")]
async fn session(pool: &PgPool, user: &User) -> String {
    format!("session={}", sessions::create(pool, user.id, time::Duration::hours(1)).await.unwrap())
}

#[cfg(feature = "google")]
#[sqlx::test(migrations = "../migrations")]
async fn google_signup_repeat_login_and_identity_survive_email_changes(pool: PgPool) {
    let (app, mock) = app(pool.clone()).await;
    for path in ["/sign-up", "/sign-in"] {
        assert!(send(&app, path, None, "").await.2.contains("Continue with Google"));
    }
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    let (_, headers, _) = send(&app, &path, None, &browser).await;
    assert_eq!(headers[header::LOCATION], "/dashboard");
    assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
    let session = cookie(&headers, "session");
    let row = sqlx::query!("SELECT id, passwordless, is_admin FROM users WHERE email = 'google-user@gmail.com'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(row.passwordless);
    assert!(!row.is_admin);
    assert!(users::verify_credentials(&pool, "google-user@gmail.com", "anything".into()).await.unwrap().is_none());
    let (_, _, settings) = send(&app, "/settings", None, &session).await;
    assert!(settings.contains("Confirm with Google"));
    // Password-free accounts are sent to the Google panel before sensitive changes.
    assert!(settings.contains("href=\"/settings#google\"") && settings.contains("id=\"google\""));
    assert!(!settings.contains("Disconnect Google"));
    assert!(!settings.contains("Current password"));
    assert_eq!(send(&app, "/auth/google", Some(""), &session).await.1[header::LOCATION], "/dashboard");
    // Completing an earlier anonymous flow must not replace a session acquired
    // in another tab while Google was open.
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    mock.claims.lock().await["sub"] = serde_json::json!("different-google-account");
    let (_, headers, _) = send(&app, &path, None, &format!("{browser}; {session}")).await;
    assert_eq!(headers[header::LOCATION], "/dashboard");
    assert!(headers.get_all(header::SET_COOKIE).iter().all(|h| !h.to_str().unwrap().starts_with("session=")));
    mock.claims.lock().await["sub"] = serde_json::json!("google-subject-123");
    mock.claims.lock().await["email"] = serde_json::json!("changed@gmail.com");
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/dashboard");
    assert_eq!(sqlx::query!("SELECT count(*) AS \"n!\" FROM users").fetch_one(&pool).await.unwrap().n, 1);
    assert_eq!(users::find(&pool, row.id).await.unwrap().unwrap().email, "google-user@gmail.com");
}

#[cfg(feature = "google")]
#[sqlx::test(migrations = "../migrations")]
async fn callbacks_require_browser_state_and_are_single_use(pool: PgPool) {
    let (app, _mock) = app(pool.clone()).await;
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    assert_eq!(send(&app, &path, None, "").await.1[header::LOCATION], "/sign-in?google=failed");
    assert_eq!(
        send(&app, &path.replace("state=", "state=wrong"), None, &browser).await.1[header::LOCATION],
        "/sign-in?google=failed"
    );
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/dashboard");
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/sign-in?google=failed");
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    sqlx::query!("UPDATE social_login_flows SET expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/sign-in?google=failed");
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    assert_eq!(
        send(&app, &format!("{path}&error=access_denied"), None, &browser).await.1[header::LOCATION],
        "/sign-in?google=cancelled"
    );
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/sign-in?google=failed");
    let request = Request::post("/auth/google").body(Body::empty()).unwrap();
    assert_eq!(app.clone().oneshot(request).await.unwrap().status(), StatusCode::FORBIDDEN);
    let request =
        Request::post("/auth/google").header(header::ORIGIN, "https://attacker.test").body(Body::empty()).unwrap();
    assert_eq!(app.oneshot(request).await.unwrap().status(), StatusCode::FORBIDDEN);
}

#[cfg(feature = "google")]
#[sqlx::test(migrations = "../migrations")]
async fn google_callback_rejects_invalid_code_lengths_before_contacting_google(pool: PgPool) {
    let (app, mock) = app(pool).await;
    for code in [None, Some(String::new()), Some("x".repeat(4097))] {
        let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
        let path = path.split("&code=").next().unwrap();
        let path = code.map_or_else(|| path.to_owned(), |code| format!("{path}&code={code}"));
        assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/sign-in?google=failed");
        assert!(mock.last_form.lock().await.is_empty());
    }
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    let path = format!("{}&code={}", path.split("&code=").next().unwrap(), "x".repeat(4096));
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/sign-in?google=failed");
    assert_eq!(mock.last_form.lock().await["code"].len(), 4096);
}

#[cfg(feature = "google")]
#[sqlx::test(migrations = "../migrations")]
async fn existing_accounts_must_explicitly_link_and_keep_their_data(pool: PgPool) {
    let owner = users::sign_up(&pool, "google-user@gmail.com", "password123".into()).await.unwrap();
    let (app, _mock) = app(pool.clone()).await;
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/sign-in?google=link");
    let session = session(&pool, &owner).await;
    // Not linked yet, but available: the contents still lead to the Google panel.
    assert!(send(&app, "/settings", None, &session).await.2.contains("href=\"#google\""));
    assert_eq!(
        send(&app, "/settings/google", Some("action=link&password=wrong"), &session).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (path, browser) = start_flow(&app, "/settings/google", "action=link&password=password123", &session).await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/settings?google=linked");
    assert!(send(&app, "/settings?google=linked", None, &session).await.2.contains("next time you sign in"));
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/dashboard");
    assert_eq!(sqlx::query!("SELECT count(*) AS \"n!\" FROM users").fetch_one(&pool).await.unwrap().n, 1);
    // A different account cannot steal the identity, even after its own password check.
    let other = users::sign_up(&pool, "other@example.com", "password123".into()).await.unwrap();
    let other_session = self::session(&pool, &other).await;
    let (path, browser) =
        start_flow(&app, "/settings/google", "action=link&password=password123", &other_session).await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/settings?google=conflict");
    assert!(send(&app, "/settings?google=conflict", None, &other_session).await.2.contains("already connected"));
    let (path, browser) = start_flow(&app, "/settings/google", "action=link&password=password123", &session).await;
    assert_eq!(
        send(&app, &path, None, &browser.replace(&session, &other_session)).await.1[header::LOCATION],
        "/sign-in?google=failed"
    );
    let second_owner_session = self::session(&pool, &owner).await;
    let (path, browser) = start_flow(&app, "/settings/google", "action=link&password=password123", &session).await;
    assert_eq!(
        send(&app, &path, None, &browser.replace(&session, &second_owner_session)).await.1[header::LOCATION],
        "/sign-in?google=failed"
    );
    app_auth::two_factor::start_challenge(&pool, owner.id).await.unwrap();
    assert_eq!(
        send(&app, "/settings/google", Some("action=disconnect&password=password123"), &session).await.1
            [header::LOCATION],
        "/settings?google=disconnected"
    );
    assert!(send(&app, "/settings?google=disconnected", None, &session).await.2.contains("Google is disconnected"));
    assert_eq!(
        sqlx::query!("SELECT count(*) AS \"n!\" FROM two_factor_challenges WHERE user_id = $1", owner.id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .n,
        0
    );
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/sign-in?google=link");
}

#[cfg(feature = "google")]
#[sqlx::test(migrations = "../migrations")]
async fn google_settings_accept_maximum_passwords_and_reject_unknown_actions(pool: PgPool) {
    let password = "p".repeat(password::MAX_LEN);
    let owner = users::sign_up(&pool, "long-password@example.com", password.clone()).await.unwrap();
    let session = session(&pool, &owner).await;
    let (app, _mock) = app(pool).await;
    assert_eq!(
        send(&app, "/settings/google", Some("action=link&password=wrong"), &session).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    for action in ["unknown", "confirm", ""] {
        let form = format!("action={action}&password={password}");
        assert_eq!(send(&app, "/settings/google", Some(&form), &session).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let (path, browser) =
        start_flow(&app, "/settings/google", &format!("action=link&password={password}"), &session).await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/settings?google=linked");
    assert_eq!(
        send(&app, "/passkeys/register/options", Some(&format!("password={password}")), &session).await.0,
        StatusCode::OK
    );
    assert_eq!(
        send(&app, "/settings/two-factor", Some(&format!("action=begin&password={password}")), &session).await.0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        send(
            &app,
            "/settings",
            Some(&format!("action=profile&name=Owner&email=changed%40example.com&password={password}")),
            &session
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        send(&app, "/settings", Some(&format!("action=delete&confirmation=DELETE&password={password}")), &session)
            .await
            .0,
        StatusCode::SEE_OTHER
    );
}

#[cfg(feature = "google")]
#[sqlx::test(migrations = "../migrations")]
async fn passwordless_sensitive_actions_need_confirmation_on_same_session(pool: PgPool) {
    let (app, mock) = app(pool.clone()).await;
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    let headers = send(&app, &path, None, &browser).await.1;
    let session = cookie(&headers, "session");
    let row =
        sqlx::query!("SELECT id FROM users WHERE email = 'google-user@gmail.com'").fetch_one(&pool).await.unwrap();
    let user = users::find(&pool, row.id).await.unwrap().unwrap();
    let second_session = self::session(&pool, &user).await;
    for (path, form) in [
        ("/settings", "action=profile&name=Name&email=changed%40gmail.com"),
        ("/settings", "action=delete&confirmation=DELETE"),
        ("/passkeys/register/options", ""),
        ("/settings/two-factor", "action=begin"),
    ] {
        assert_eq!(send(&app, path, Some(form), &session).await.0, StatusCode::UNPROCESSABLE_ENTITY, "{path}");
    }
    assert_eq!(
        send(&app, "/settings/google", Some("action=disconnect"), &session).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (path, browser) = start_flow(&app, "/settings/google", "action=confirm", &session).await;
    mock.claims.lock().await["sub"] = serde_json::json!("another-google-account");
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/settings?google=wrong-account");
    assert!(send(&app, "/settings?google=wrong-account", None, &session).await.2.contains("Choose the Google account"));
    mock.claims.lock().await["sub"] = serde_json::json!("google-subject-123");
    let (path, browser) = start_flow(&app, "/settings/google", "action=confirm", &session).await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/settings?google=confirmed");
    assert!(
        send(&app, "/settings?google=confirmed", None, &session)
            .await
            .2
            .contains("sensitive changes for the next five minutes")
    );
    assert_eq!(
        send(&app, "/passkeys/register/options", Some(""), &second_session).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(send(&app, "/passkeys/register/options", Some(""), &session).await.0, StatusCode::OK);
    assert_eq!(send(&app, "/settings/two-factor", Some("action=begin"), &session).await.0, StatusCode::SEE_OTHER);
    assert_eq!(
        send(&app, "/settings", Some("action=profile&name=Name&email=changed%40gmail.com"), &session).await.0,
        StatusCode::SEE_OTHER
    );
    sqlx::query!("UPDATE social_login_confirmations SET expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        send(&app, "/settings", Some("action=delete&confirmation=DELETE"), &session).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (path, browser) = start_flow(&app, "/settings/google", "action=confirm", &session).await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/settings?google=confirmed");
    assert_eq!(
        send(&app, "/settings", Some("action=delete&confirmation=DELETE"), &session).await.0,
        StatusCode::SEE_OTHER
    );
    assert!(users::find(&pool, user.id).await.unwrap().is_none());
    assert_eq!(
        sqlx::query!("SELECT count(*) AS \"n!\" FROM social_login_confirmations").fetch_one(&pool).await.unwrap().n,
        0
    );
}

#[cfg(feature = "google")]
#[sqlx::test(migrations = "../migrations")]
async fn google_signin_respects_existing_two_factor(pool: PgPool) {
    let (app, _mock) = app(pool.clone()).await;
    let owner = users::sign_up(&pool, "google-user@gmail.com", "password123".into()).await.unwrap();
    let session = session(&pool, &owner).await;
    let (path, browser) = start_flow(&app, "/settings/google", "action=link&password=password123", &session).await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/settings?google=linked");
    app_auth::two_factor::begin(&pool, owner.id, crate::views::SITE_NAME, &owner.email).await.unwrap();
    sqlx::query!("UPDATE totp_secrets SET confirmed_at = now() WHERE user_id = $1", owner.id)
        .execute(&pool)
        .await
        .unwrap();
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    let (_, headers, _) = send(&app, &path, None, &browser).await;
    assert_eq!(headers[header::LOCATION], "/sign-in/two-factor");
    assert!(headers.get_all(header::SET_COOKIE).iter().all(|h| !h.to_str().unwrap().starts_with("session=")));
}

#[cfg(feature = "google")]
#[sqlx::test(migrations = "../migrations")]
async fn non_authoritative_emails_cannot_create_passwordless_accounts(pool: PgPool) {
    let (app, mock) = app(pool.clone()).await;
    mock.claims.lock().await["email"] = serde_json::json!("user@third-party.test");
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/sign-in?google=link");
    assert_eq!(sqlx::query!("SELECT count(*) AS \"n!\" FROM users").fetch_one(&pool).await.unwrap().n, 0);
}

#[cfg(feature = "google")]
#[sqlx::test(migrations = "../migrations")]
async fn simultaneous_first_signins_share_one_account(pool: PgPool) {
    let (app, _mock) = app(pool.clone()).await;
    let (path1, browser1) = start_flow(&app, "/auth/google", "", "").await;
    let (path2, browser2) = start_flow(&app, "/auth/google", "", "").await;
    let (one, two) = tokio::join!(send(&app, &path1, None, &browser1), send(&app, &path2, None, &browser2));
    assert_eq!(one.1[header::LOCATION], "/dashboard");
    assert_eq!(two.1[header::LOCATION], "/dashboard");
    assert_eq!(sqlx::query!("SELECT count(*) AS \"n!\" FROM users").fetch_one(&pool).await.unwrap().n, 1);
}

#[cfg(feature = "google")]
#[sqlx::test(migrations = "../migrations")]
async fn google_disconnect_cannot_leave_an_inflight_login_session(pool: PgPool) {
    disconnect_during_login(pool, false).await;
}

#[cfg(feature = "google")]
#[sqlx::test(migrations = "../migrations")]
async fn google_disconnect_cannot_leave_an_inflight_two_factor_challenge(pool: PgPool) {
    disconnect_during_login(pool, true).await;
}

#[cfg(feature = "google")]
async fn disconnect_during_login(pool: PgPool, challenge: bool) {
    let owner = users::sign_up(&pool, "owner@example.com", "correct-horse".into()).await.unwrap();
    let session = session(&pool, &owner).await;
    let (app, _mock) = app(pool.clone()).await;
    let (path, browser) = start_flow(&app, "/settings/google", "action=link&password=correct-horse", &session).await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/settings?google=linked");
    if challenge {
        app_auth::two_factor::begin(&pool, owner.id, crate::views::SITE_NAME, &owner.email).await.unwrap();
        sqlx::query!("UPDATE totp_secrets SET confirmed_at = now() WHERE user_id = $1", owner.id)
            .execute(&pool)
            .await
            .unwrap();
    }
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    // Hold the same lock as disconnect, after credential lookup but before the
    // session INSERT can acquire its foreign-key lock on the user.
    let mut disconnect = pool.begin().await.unwrap();
    sqlx::query!("SELECT id FROM users WHERE id = $1 FOR UPDATE", owner.id).fetch_one(&mut *disconnect).await.unwrap();
    let callback = tokio::spawn(async move { send(&app, &path, None, &browser).await });
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let insert = if challenge { "INSERT INTO two_factor_challenges%" } else { "INSERT INTO sessions%" };
            let waiting = sqlx::query!(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock' AND query LIKE $1) AS \"waiting!\"", insert
            ).fetch_one(&pool).await.unwrap().waiting;
            if waiting { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("callback reached session insertion");
    sqlx::query!("DELETE FROM social_identities WHERE user_id = $1", owner.id).execute(&mut *disconnect).await.unwrap();
    sqlx::query!("DELETE FROM sessions WHERE user_id = $1", owner.id).execute(&mut *disconnect).await.unwrap();
    sqlx::query!("DELETE FROM two_factor_challenges WHERE user_id = $1", owner.id)
        .execute(&mut *disconnect)
        .await
        .unwrap();
    disconnect.commit().await.unwrap();
    let response = callback.await.unwrap();
    assert_eq!(
        sqlx::query!("SELECT count(*) AS \"n!\" FROM two_factor_challenges WHERE user_id = $1", owner.id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .n,
        0
    );
    assert_eq!(response.1[header::LOCATION], "/sign-in?google=failed");
    assert_eq!(sqlx::query!("SELECT count(*) AS \"n!\" FROM sessions").fetch_one(&pool).await.unwrap().n, 0);
}

#[cfg(feature = "google")]
#[sqlx::test(migrations = "../migrations")]
async fn passwordless_accounts_keep_their_last_provider(pool: PgPool) {
    let (app, _mock) = app(pool.clone()).await;
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    let session = cookie(&send(&app, &path, None, &browser).await.1, "session");
    // Even confirmed, the only provider stays connected.
    let (path, browser) = start_flow(&app, "/settings/google", "action=confirm", &session).await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/settings?google=confirmed");
    let (status, _, page) = send(&app, "/settings/google", Some("action=disconnect"), &session).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(page.contains("Keep Google connected"));
    #[cfg(feature = "github")]
    {
        // With a second provider linked, Google can go after a fresh confirmation.
        let id = sqlx::query_scalar!("SELECT id FROM users WHERE email = 'google-user@gmail.com'")
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query!(
            "INSERT INTO social_identities (user_id, provider, subject) VALUES ($1, 'github', 'github-subject')",
            id
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(send(&app, "/settings", None, &session).await.2.contains("Disconnect Google"));
        sqlx::query!("DELETE FROM social_login_confirmations").execute(&pool).await.unwrap();
        let (status, _, page) = send(&app, "/settings/google", Some("action=disconnect"), &session).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(page.contains(CONFIRM_MESSAGE));
        let (path, browser) = start_flow(&app, "/settings/google", "action=confirm", &session).await;
        assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/settings?google=confirmed");
        assert_eq!(
            send(&app, "/settings/google", Some("action=disconnect"), &session).await.1[header::LOCATION],
            "/settings?google=disconnected"
        );
        let left = sqlx::query_scalar!("SELECT provider FROM social_identities WHERE user_id = $1", id)
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(left, ["github"]);
    }
}

#[test]
fn notices_and_paths_cover_every_supported_provider() {
    let query = |id: &str, kind: &str| std::collections::HashMap::from([(id.to_owned(), kind.to_owned())]);
    assert!(sign_in_notice(&query("unknown", "failed")).is_none());
    for &(id, name) in SUPPORTED {
        assert_eq!(
            sign_in_notice(&query(id, "cancelled")),
            Some(format!("{name} sign-in was cancelled. You can try again."))
        );
        assert_eq!(
            settings_notice(&query(id, "linked")),
            Some(format!("{name} is connected. You can use it next time you sign in."))
        );
        assert!(sign_in_notice(&query(id, "other")).is_none());
        for (path, form, private) in [
            (format!("/auth/{id}"), true, true),
            (format!("/settings/{id}"), true, true),
            (format!("/auth/{id}/callback"), false, true),
        ] {
            assert_eq!(is_form_path(&path), form, "{path}");
            assert_eq!(provider_path(&path).is_some(), private, "{path}");
        }
    }
    for path in ["/auth/unknown", "/settings/two-factor", "/settings"] {
        assert!(!is_form_path(path) && provider_path(path).is_none(), "{path}");
    }
}

#[cfg(feature = "github")]
#[sqlx::test(migrations = "../migrations")]
async fn github_signup_creates_a_passwordless_account_and_signs_back_in(pool: PgPool) {
    let (app, github) = github_app(pool.clone()).await;
    let (_, _, page) = send(&app, "/sign-up", None, "").await;
    assert!(page.contains("or continue with GitHub."));
    assert!(page.contains("action=\"/auth/github\"") && page.contains("class=\"github-logo\""));
    let (path, browser) = start_github_flow(&app, "/auth/github", "", "").await;
    let (_, headers, _) = send(&app, &path, None, &browser).await;
    assert_eq!(headers[header::LOCATION], "/dashboard");
    assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
    let session = cookie(&headers, "session");
    let row = sqlx::query!("SELECT u.id, u.passwordless, i.subject FROM users u JOIN social_identities i ON i.user_id = u.id AND i.provider = 'github' WHERE u.email = 'octocat@example.com'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(row.passwordless);
    assert_eq!(row.subject, "583231");
    assert_eq!(*github.revoked.lock().await, ["gho_test_access_token"]);
    let settings = send(&app, "/settings", None, &session).await.2;
    assert!(settings.contains("Confirm with GitHub") && settings.contains("href=\"/settings#github\""));
    // A renamed login or changed email does not create a second account.
    *github.emails.lock().await = serde_json::json!([{"email": "new@example.com", "primary": true, "verified": true}]);
    let (path, browser) = start_github_flow(&app, "/auth/github", "", "").await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/dashboard");
    assert_eq!(sqlx::query!("SELECT count(*) AS \"n!\" FROM users").fetch_one(&pool).await.unwrap().n, 1);
    // Without a deliverable verified primary email, a new GitHub user is sent to email signup.
    *github.user.lock().await = serde_json::json!({"id": 99});
    *github.emails.lock().await =
        serde_json::json!([{"email": "x@users.noreply.github.com", "primary": true, "verified": true}]);
    let (path, browser) = start_github_flow(&app, "/auth/github", "", "").await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/sign-in?github=link");
    assert!(send(&app, "/sign-in?github=link", None, "").await.2.contains("then connect GitHub in account settings"));
}

#[cfg(all(feature = "google", feature = "github"))]
#[sqlx::test(migrations = "../migrations")]
async fn google_account_links_github_after_confirmation_and_signs_in_with_either(pool: PgPool) {
    let (app, _google, _github) = app_with_github(pool.clone()).await;
    let (path, browser) = start_flow(&app, "/auth/google", "", "").await;
    let session = cookie(&send(&app, &path, None, &browser).await.1, "session");
    // Linking needs a fresh confirmation: this account has no password.
    let (status, _, page) = send(&app, "/settings/github", Some("action=link"), &session).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(page.contains(CONFIRM_MESSAGE));
    let (path, browser) = start_flow(&app, "/settings/google", "action=confirm", &session).await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/settings?google=confirmed");
    let (path, browser) = start_github_flow(&app, "/settings/github", "action=link", &session).await;
    assert_eq!(send(&app, &path, None, &browser).await.1[header::LOCATION], "/settings?github=linked");
    assert!(send(&app, "/settings?github=linked", None, &session).await.2.contains("GitHub is connected."));
    // GitHub's email differs from the Google account's; the subject decides.
    let (path, browser) = start_github_flow(&app, "/auth/github", "", "").await;
    let (_, headers, _) = send(&app, &path, None, &browser).await;
    assert_eq!(headers[header::LOCATION], "/dashboard");
    assert_eq!(sqlx::query!("SELECT count(*) AS \"n!\" FROM users").fetch_one(&pool).await.unwrap().n, 1);
    // With two providers, either can be disconnected after a confirmation.
    let settings = send(&app, "/settings", None, &session).await.2;
    assert!(settings.contains("Disconnect GitHub") && settings.contains("Disconnect Google"));
}
