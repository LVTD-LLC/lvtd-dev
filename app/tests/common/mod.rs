#![allow(dead_code)]
#[cfg(feature = "auth")]
use lvtd_dev as app;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;

#[derive(Default)]
struct Inbox {
    codes: HashMap<String, String>,
    mails: Vec<(String, String, String)>,
    fail_next: bool,
}
#[derive(Clone, Default)]
pub struct Mailbox(Arc<Mutex<Inbox>>);

/// Attach the fixture handle to a test-only route. No process-global mail state.
pub fn with_mailbox(router: Router, mailbox: Mailbox) -> Router {
    router.route("/__test/mailbox", axum::routing::get(move || async move { axum::Extension(mailbox) }))
}
async fn mailbox(app: &Router) -> Mailbox {
    let response = app.clone().oneshot(Request::get("/__test/mailbox").body(Body::empty()).unwrap()).await.unwrap();
    response.extensions().get::<Mailbox>().expect("mail fixture attached").clone()
}

pub fn mail_double() -> (String, Mailbox) {
    let mailbox = Mailbox::default();
    let server_mailbox = mailbox.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route(
                    "/messages",
                    axum::routing::post(
                        |axum::extract::State(mailbox): axum::extract::State<Mailbox>,
                         axum::extract::Form(mail): axum::extract::Form<Mail>| async move {
                            let mut inbox = mailbox.0.lock().unwrap();
                            if std::mem::take(&mut inbox.fail_next) {
                                return (StatusCode::SERVICE_UNAVAILABLE, "{\"message\":\"try later\"}");
                            }
                            if let Some(code) = mail.text.split("code is: ").nth(1).and_then(|rest| rest.lines().next())
                            {
                                inbox.codes.insert(mail.to.clone(), code.to_string());
                            }
                            inbox.mails.push((mail.to, mail.subject, mail.text));
                            (StatusCode::OK, "{\"id\":\"test\",\"message\":\"Queued\"}")
                        },
                    ),
                )
                .with_state(server_mailbox),
        )
        .await
        .unwrap();
    });
    (format!("http://{address}/messages"), mailbox)
}
#[derive(serde::Deserialize)]
struct Mail {
    to: String,
    subject: String,
    text: String,
}
#[cfg(feature = "auth")]
pub fn mail_app((mut state, router): (app::SharedState, Router)) -> (app::SharedState, Router) {
    drop(router);
    let (endpoint, mailbox) = mail_double();
    std::sync::Arc::get_mut(&mut state).unwrap().email = Some(
        app::verification::Client::with_endpoint(
            app::verification::Config {
                api_key: "test".into(),
                domain: "mg.example.com".into(),
                from: "noreply@example.com".into(),
                hash_key: "test-only-long-key-not-production-12345".into(),
            },
            endpoint,
        )
        .unwrap(),
    );
    let router = app::routes::router(state.clone());
    (state, with_mailbox(router, mailbox))
}
pub async fn code(app: &Router, email: &str) -> String {
    mailbox(app).await.0.lock().unwrap().codes.get(email).expect("delivered email").clone()
}
/// Messages delivered to `to`, as (subject, text).
pub async fn mails(app: &Router, to: &str) -> Vec<(String, String)> {
    mailbox(app)
        .await
        .0
        .lock()
        .unwrap()
        .mails
        .iter()
        .filter(|(t, _, _)| t == to)
        .map(|(_, s, b)| (s.clone(), b.clone()))
        .collect()
}
pub async fn fail_next_mail(app: &Router) {
    mailbox(app).await.0.lock().unwrap().fail_next = true;
}
pub async fn complete(app: &Router, cookie: &str, email: &str) -> axum::response::Response {
    let code = code(app, email).await;
    app.clone()
        .oneshot(
            Request::post("/verify-email")
                .header("traceparent", "00-11111111111111111111111111111111-2222222222222222-01")
                .header(header::ORIGIN, "http://test.local")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, cookie)
                .body(Body::from(format!("code={code}")))
                .unwrap(),
        )
        .await
        .unwrap()
}
/// A browser-style `multipart/form-data` POST with one file field.
pub fn multipart(
    path: &str,
    field: &str,
    filename: &str,
    content_type: &str,
    data: &[u8],
    cookie: Option<&str>,
) -> Request<Body> {
    let boundary = "----lvtd-dev-test-boundary";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\n\
             Content-Type: {content_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let mut request = Request::post(path)
        .header(header::ORIGIN, "http://test.local")
        .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"));
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    request.body(Body::from(body)).unwrap()
}

/// Objects an S3 double holds, as `bucket/key` → (content type, bytes).
pub type Objects = std::sync::Arc<Mutex<HashMap<String, (String, Vec<u8>)>>>;
pub const S3_KEY_ID: &str = "AKIATEST";

/// A path-style S3 endpoint that stores objects in memory. It checks that
/// requests are SigV4-signed with the test key (the shared storage crate's
/// own tests verify signatures byte for byte).
pub async fn s3_double() -> (String, Objects) {
    use axum::extract::{Path, State};
    use axum::http::HeaderMap;
    let objects: Objects = Default::default();
    fn signed(headers: &HeaderMap) -> Result<(), StatusCode> {
        let authorization = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or_default();
        let ok = authorization.starts_with(&format!("AWS4-HMAC-SHA256 Credential={S3_KEY_ID}/"))
            && authorization.contains("SignedHeaders=")
            && headers.contains_key("x-amz-content-sha256")
            && headers.contains_key("x-amz-date");
        if ok { Ok(()) } else { Err(StatusCode::FORBIDDEN) }
    }
    let router =
        Router::new()
            .route(
                "/{bucket}/{*key}",
                axum::routing::put(
                    |State(objects): State<Objects>,
                     Path((bucket, key)): Path<(String, String)>,
                     headers: HeaderMap,
                     body: axum::body::Bytes| async move {
                        signed(&headers)?;
                        let content_type = headers[header::CONTENT_TYPE].to_str().unwrap().to_owned();
                        objects.lock().unwrap().insert(format!("{bucket}/{key}"), (content_type, body.to_vec()));
                        Ok::<_, StatusCode>(StatusCode::OK)
                    },
                )
                .get(
                    |State(objects): State<Objects>,
                     Path((bucket, key)): Path<(String, String)>,
                     headers: HeaderMap| async move {
                        signed(&headers)?;
                        match objects.lock().unwrap().get(&format!("{bucket}/{key}")) {
                            Some((content_type, bytes)) => {
                                Ok::<_, StatusCode>(([(header::CONTENT_TYPE, content_type.clone())], bytes.clone()))
                            }
                            None => Err(StatusCode::NOT_FOUND),
                        }
                    },
                )
                .delete(
                    |State(objects): State<Objects>,
                     Path((bucket, key)): Path<(String, String)>,
                     headers: HeaderMap| async move {
                        signed(&headers)?;
                        objects.lock().unwrap().remove(&format!("{bucket}/{key}"));
                        Ok::<_, StatusCode>(StatusCode::NO_CONTENT)
                    },
                ),
            )
            .with_state(objects.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{address}"), objects)
}
