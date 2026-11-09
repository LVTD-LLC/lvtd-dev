#![no_main]

use app_auth::webauthn::{self, PublicKey, RelyingParty};
use libfuzzer_sys::fuzz_target;
use std::sync::LazyLock;

static RP: LazyLock<RelyingParty> =
    LazyLock::new(|| RelyingParty::from_site_url("https://example.com", "Fuzz app").unwrap());
const CLIENT: &[u8] = br#"{"type":"webauthn.create","challenge":"AA","origin":"https://example.com"}"#;

fuzz_target!(|data: &[u8]| {
    let _ = webauthn::client_data_challenge(data);
    if let Ok(key) = PublicKey::parse(data) {
        assert!(webauthn::ALGORITHMS.contains(&key.algorithm));
        let _ = key.verify(b"fuzz message", data);
    }
    // Valid client data gets arbitrary CBOR past the ceremony's first gate.
    if let Ok(registration) = webauthn::verify_registration(&RP, &[0], &[0], CLIENT, data) {
        assert_eq!(registration.credential_id, [0]);
        assert!(PublicKey::parse(&registration.public_key).is_ok());
    }
});
