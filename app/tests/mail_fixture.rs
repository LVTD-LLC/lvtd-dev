//! Mail doubles must remain independent when parallel tests reuse an address.
mod common;

#[tokio::test]
async fn mail_fixtures_isolate_codes_messages_and_delivery_failures() {
    let (first_url, first_mailbox) = common::mail_double();
    let (second_url, second_mailbox) = common::mail_double();
    let first = common::with_mailbox(axum::Router::new(), first_mailbox);
    let second = common::with_mailbox(axum::Router::new(), second_mailbox);
    let client = reqwest::Client::new();
    let send =
        |url, code| client.post(url).form(&[("to", "same@example.com"), ("subject", "Verify"), ("text", code)]).send();
    let (a, b) = tokio::join!(send(&first_url, "code is: FIRST"), send(&second_url, "code is: SECOND"));
    assert_eq!(a.unwrap().status(), 200);
    assert_eq!(b.unwrap().status(), 200);
    assert_eq!(common::code(&first, "same@example.com").await, "FIRST");
    assert_eq!(common::code(&second, "same@example.com").await, "SECOND");
    common::fail_next_mail(&first).await;
    assert_eq!(send(&second_url, "code is: OTHER").await.unwrap().status(), 200);
    assert_eq!(send(&first_url, "code is: REJECTED").await.unwrap().status(), 503);
    assert_eq!(common::code(&first, "same@example.com").await, "FIRST");
    assert_eq!(common::mails(&first, "same@example.com").await.len(), 1);
    assert_eq!(common::mails(&second, "same@example.com").await.len(), 2);
    assert_eq!(send(&first_url, "code is: RETRY").await.unwrap().status(), 200);
    assert_eq!(common::code(&first, "same@example.com").await, "RETRY");
}
