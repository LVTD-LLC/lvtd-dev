//! Time-based one-time passwords (RFC 6238 over RFC 4226 HOTP), as every
//! authenticator app implements them: HMAC-SHA1, six digits, 30-second
//! steps. Pure: no I/O and no clock. The HMAC comes from `ring`, which is
//! already in the tree for passkeys.
//!
//! SHA-1 is the only algorithm all authenticator apps support; HOTP uses it
//! as a MAC, where its collision weakness does not apply.

use ring::hmac;
use time::OffsetDateTime;

/// 160 bits, the key size RFC 4226 recommends for HMAC-SHA1.
pub const SECRET_LEN: usize = 20;
pub const DIGITS: usize = 6;
pub const STEP_SECONDS: i64 = 30;
/// Steps either side of now that are still accepted (clock drift, slow typing).
pub const SKEW: i64 = 1;

/// A fresh random secret.
pub fn generate_secret() -> anyhow::Result<Vec<u8>> {
    let mut secret = vec![0u8; SECRET_LEN];
    getrandom::fill(&mut secret)?;
    Ok(secret)
}

/// The time step `at` falls in.
pub fn step_at(at: OffsetDateTime) -> i64 {
    at.unix_timestamp().div_euclid(STEP_SECONDS)
}

fn hotp(secret: &[u8], counter: u64, digits: usize) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let tag = hmac::sign(&key, &counter.to_be_bytes());
    let mac = tag.as_ref();
    let offset = (mac[mac.len() - 1] & 0x0f) as usize;
    let binary = u32::from_be_bytes([mac[offset] & 0x7f, mac[offset + 1], mac[offset + 2], mac[offset + 3]]);
    let value = binary % 10u32.pow(digits as u32);
    format!("{value:0digits$}")
}

/// The six-digit code for `step`.
pub fn code(secret: &[u8], step: i64) -> String {
    hotp(secret, step as u64, DIGITS)
}

/// The user's input with spaces removed, if it has the shape of a code.
pub fn normalize_code(input: &str) -> Option<String> {
    let digits: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    (digits.len() == DIGITS && digits.bytes().all(|b| b.is_ascii_digit())).then_some(digits)
}

/// The step `code` is valid for, if it matches one within `SKEW` of `now_step`
/// and is later than `after_step` (so a code is never accepted twice).
/// Every candidate step is compared in full whether or not an earlier one
/// matched, so the time taken does not depend on the answer.
pub fn matching_step(secret: &[u8], code: &str, now_step: i64, after_step: i64) -> Option<i64> {
    let mut matched = None;
    for step in (now_step - SKEW)..=(now_step + SKEW) {
        if equal_constant_time(self::code(secret, step).as_bytes(), code.as_bytes())
            && step > after_step
            && matched.is_none()
        {
            matched = Some(step);
        }
    }
    matched
}

fn equal_constant_time(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Base32 (RFC 4648, no padding), the alphabet authenticator apps accept
/// for manual entry.
pub fn base32_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

/// Inverse of [`base32_encode`]; case-insensitive, ignores spaces and `=`.
pub fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for c in text.chars().filter(|c| !c.is_whitespace() && *c != '=') {
        let value = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u32 - 'A' as u32,
            c @ '2'..='7' => c as u32 - '2' as u32 + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// `otpauth://totp/...`, the URL authenticator apps read from the QR code.
pub fn otpauth_url(issuer: &str, account: &str, secret: &[u8]) -> String {
    format!(
        "otpauth://totp/{issuer}:{account}?secret={secret}&issuer={issuer}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECONDS}",
        issuer = percent_encode(issuer),
        account = percent_encode(account),
        secret = base32_encode(secret),
    )
}

fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RFC_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn hotp_matches_the_rfc_4226_vectors() {
        let expected =
            ["755224", "287082", "359152", "969429", "338314", "254676", "287922", "162583", "399871", "520489"];
        for (counter, code) in expected.iter().enumerate() {
            assert_eq!(hotp(RFC_SECRET, counter as u64, 6), *code, "counter {counter}");
        }
    }

    #[test]
    fn totp_matches_the_rfc_6238_vectors() {
        // RFC 6238 lists eight digits; the six-digit code is the last six.
        for (seconds, eight) in [(59, "94287082"), (1_111_111_109, "07081804"), (1_234_567_890, "89005924")] {
            let at = OffsetDateTime::from_unix_timestamp(seconds).unwrap();
            assert_eq!(hotp(RFC_SECRET, step_at(at) as u64, 8), eight);
            assert_eq!(code(RFC_SECRET, step_at(at)), &eight[2..]);
        }
    }

    #[test]
    fn codes_match_within_one_step_and_only_once() {
        let now = 50_000_000;
        for step in [now - 1, now, now + 1] {
            assert_eq!(matching_step(RFC_SECRET, &code(RFC_SECRET, step), now, 0), Some(step));
        }
        assert_eq!(matching_step(RFC_SECRET, &code(RFC_SECRET, now - 2), now, 0), None);
        assert_eq!(matching_step(RFC_SECRET, &code(RFC_SECRET, now + 2), now, 0), None);
        assert_eq!(matching_step(RFC_SECRET, &code(RFC_SECRET, now), now, now), None, "already used");
        assert_eq!(matching_step(RFC_SECRET, &code(RFC_SECRET, now - 1), now, now), None, "older than last use");
        assert_eq!(matching_step(RFC_SECRET, "000000", now, 0).is_some(), code(RFC_SECRET, now) == "000000");
    }

    #[test]
    fn user_input_is_normalized_and_validated() {
        assert_eq!(normalize_code(" 123 456 ").as_deref(), Some("123456"));
        assert_eq!(normalize_code("12345"), None);
        assert_eq!(normalize_code("1234567"), None);
        assert_eq!(normalize_code("12345a"), None);
        assert_eq!(normalize_code("１２３４５６"), None, "ASCII digits only");
    }

    #[test]
    fn base32_round_trips_and_matches_known_encodings() {
        assert_eq!(base32_encode(b""), "");
        assert_eq!(base32_encode(b"f"), "MY");
        assert_eq!(base32_encode(b"fo"), "MZXQ");
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_encode(RFC_SECRET), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert_eq!(base32_decode("mzxw6ytboi").as_deref(), Some(&b"foobar"[..]));
        assert_eq!(base32_decode("MZXW 6YTB OI==").as_deref(), Some(&b"foobar"[..]));
        assert_eq!(base32_decode("MZXW6YTB0I"), None, "0 is not in the alphabet");
        let secret = generate_secret().unwrap();
        assert_eq!(secret.len(), SECRET_LEN);
        assert_eq!(base32_decode(&base32_encode(&secret)).unwrap(), secret);
        assert_ne!(generate_secret().unwrap(), secret);
    }

    #[test]
    fn otpauth_url_escapes_the_label_and_names_the_parameters() {
        let url = otpauth_url("Ship Rust", "ann@example.com", b"foobar");
        assert_eq!(
            url,
            "otpauth://totp/Ship%20Rust:ann%40example.com?secret=MZXW6YTBOI&issuer=Ship%20Rust&algorithm=SHA1&digits=6&period=30"
        );
    }
}
