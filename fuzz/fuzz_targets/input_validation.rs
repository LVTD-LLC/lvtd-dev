#![no_main]

use app_auth::{totp, webauthn};
use app_core::{http, storage};
use axum::http::{HeaderMap, HeaderValue, header};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Round trips give us a correctness oracle even for arbitrary bytes.
    assert_eq!(totp::base32_decode(&totp::base32_encode(data)).unwrap(), data);
    assert_eq!(webauthn::b64url_decode(&webauthn::b64url_encode(data)).unwrap(), data);
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = totp::base32_decode(text);
        let _ = webauthn::b64url_decode(text);
        let _ = webauthn::RelyingParty::from_site_url(text, "Fuzz app");
        if let Some(code) = totp::normalize_code(text) {
            assert_eq!(code.len(), 6);
            assert!(code.bytes().all(|b| b.is_ascii_digit()));
            assert_eq!(totp::normalize_code(&code).as_ref(), Some(&code));
        }
        if storage::is_valid_key(text) {
            assert!(!text.is_empty() && text.len() <= storage::MAX_KEY_LEN);
            assert!(!text.starts_with('/') && !text.contains('\\'));
            assert!(text.split('/').all(|part| !matches!(part, "" | "." | "..")));
        }
        let _ = storage::content_type_for(text);
    }
    if let Ok(value) = HeaderValue::from_bytes(data) {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, value);
        let _ = http::prefers_markdown(&headers);
    }
});
