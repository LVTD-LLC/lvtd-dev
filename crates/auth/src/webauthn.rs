//! WebAuthn relying-party verification for passkeys, without a native
//! crypto library: signatures are checked with `ring`, which rustls already
//! brings into every LVTD binary.
//!
//! Scope, on purpose:
//! - registration with `attestation: "none"` (the attestation statement is
//!   ignored; consumer passkeys do not need device attestation)
//! - ES256, RS256 and EdDSA (Ed25519) credentials, the algorithms every
//!   platform authenticator and security key supports
//! - user verification required on both ceremonies, because a passkey is the
//!   only factor at sign-in
//!
//! Everything here is pure: bytes in, verified facts out. [`crate::passkeys`]
//! owns challenges, storage and the SQL.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// COSE algorithm identifiers offered to authenticators, in preference order.
pub const ALGORITHMS: [i64; 3] = [ES256, RS256, EDDSA];
pub const ES256: i64 = -7;
pub const RS256: i64 = -257;
pub const EDDSA: i64 = -8;

const FLAG_USER_PRESENT: u8 = 0x01;
const FLAG_USER_VERIFIED: u8 = 0x04;
const FLAG_BACKUP_ELIGIBLE: u8 = 0x08;
const FLAG_BACKED_UP: u8 = 0x10;
const FLAG_ATTESTED: u8 = 0x40;
const FLAG_EXTENSIONS: u8 = 0x80;

/// Who the browser is talking to. Derived from `SITE_URL`; no extra config.
#[derive(Clone, Debug)]
pub struct RelyingParty {
    /// The effective domain, e.g. `example.com` (no scheme, no port).
    pub id: String,
    /// Shown by the browser and password managers next to the passkey.
    pub name: String,
    /// The exact origin the browser must report, e.g. `https://example.com`.
    pub origin: String,
}

impl RelyingParty {
    /// `site_url` is `Config::site_url`: `http(s)://host[:port]`, optionally
    /// with a path, which is ignored.
    pub fn from_site_url(site_url: &str, name: &str) -> anyhow::Result<Self> {
        let (scheme, rest) = site_url
            .split_once("://")
            .filter(|(scheme, _)| matches!(*scheme, "http" | "https"))
            .ok_or_else(|| anyhow::anyhow!("SITE_URL must start with http:// or https://"))?;
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default().to_ascii_lowercase();
        let host = authority.rsplit_once(':').map_or(authority.as_str(), |(host, _)| host).to_string();
        anyhow::ensure!(
            !host.is_empty() && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'),
            "SITE_URL host {host:?} cannot be a WebAuthn relying party id"
        );
        Ok(Self { id: host, name: name.to_string(), origin: format!("{scheme}://{authority}") })
    }

    fn id_hash(&self) -> [u8; 32] {
        Sha256::digest(self.id.as_bytes()).into()
    }
}

/// A verification failure caused by the client's data. Messages are safe to
/// show to the user and never include the offending bytes.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Rejected(pub &'static str);

pub fn b64url_decode(text: &str) -> Result<Vec<u8>, Rejected> {
    URL_SAFE_NO_PAD.decode(text.trim_end_matches('=')).map_err(|_| Rejected("Malformed base64url data."))
}

pub fn b64url_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

// ---------------------------------------------------------------------------
// CBOR, the subset WebAuthn uses (RFC 8949): integers, byte and text
// strings, arrays and maps of definite length. Anything else is rejected.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Cbor {
    Int(i128),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Bool(bool),
    Null,
}

impl Cbor {
    /// Decode one item from the start of `bytes`; returns it and how many
    /// bytes it occupied, so callers can keep parsing what follows.
    pub(crate) fn decode(bytes: &[u8]) -> Result<(Cbor, usize), Rejected> {
        let mut reader = Reader { bytes, pos: 0 };
        let value = reader.item(0)?;
        Ok((value, reader.pos))
    }

    pub(crate) fn decode_exact(bytes: &[u8]) -> Result<Cbor, Rejected> {
        match Self::decode(bytes)? {
            (value, used) if used == bytes.len() => Ok(value),
            _ => Err(Rejected("Trailing bytes after CBOR item.")),
        }
    }

    fn get(&self, key: &Cbor) -> Option<&Cbor> {
        match self {
            Cbor::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn get_int(&self, key: i128) -> Option<i128> {
        match self.get(&Cbor::Int(key))? {
            Cbor::Int(v) => Some(*v),
            _ => None,
        }
    }

    fn get_bytes(&self, key: i128) -> Option<&[u8]> {
        match self.get(&Cbor::Int(key))? {
            Cbor::Bytes(v) => Some(v),
            _ => None,
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    const MAX_DEPTH: usize = 8;
    const MAX_ITEMS: u64 = 1024;

    fn take(&mut self, n: usize) -> Result<&[u8], Rejected> {
        let end = self.pos.checked_add(n).filter(|end| *end <= self.bytes.len()).ok_or(Rejected("Truncated CBOR."))?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn argument(&mut self, info: u8) -> Result<u64, Rejected> {
        Ok(match info {
            0..=23 => u64::from(info),
            24 => u64::from(self.take(1)?[0]),
            25 => u64::from(u16::from_be_bytes(self.take(2)?.try_into().unwrap())),
            26 => u64::from(u32::from_be_bytes(self.take(4)?.try_into().unwrap())),
            27 => u64::from_be_bytes(self.take(8)?.try_into().unwrap()),
            _ => return Err(Rejected("Indefinite-length or reserved CBOR is not allowed.")),
        })
    }

    fn item(&mut self, depth: usize) -> Result<Cbor, Rejected> {
        if depth > Self::MAX_DEPTH {
            return Err(Rejected("CBOR nests too deeply."));
        }
        let head = self.take(1)?[0];
        let (major, info) = (head >> 5, head & 0x1f);
        let argument = if major == 7 { 0 } else { self.argument(info)? };
        let length = |argument: u64| usize::try_from(argument).map_err(|_| Rejected("CBOR item too large."));
        Ok(match major {
            0 => Cbor::Int(i128::from(argument)),
            1 => Cbor::Int(-1 - i128::from(argument)),
            2 => Cbor::Bytes(self.take(length(argument)?)?.to_vec()),
            3 => Cbor::Text(
                String::from_utf8(self.take(length(argument)?)?.to_vec())
                    .map_err(|_| Rejected("CBOR text is not UTF-8."))?,
            ),
            4 => {
                if argument > Self::MAX_ITEMS {
                    return Err(Rejected("CBOR array too long."));
                }
                let mut items = Vec::with_capacity(argument.min(64) as usize);
                for _ in 0..argument {
                    items.push(self.item(depth + 1)?);
                }
                Cbor::Array(items)
            }
            5 => {
                if argument > Self::MAX_ITEMS {
                    return Err(Rejected("CBOR map too long."));
                }
                let mut entries: Vec<(Cbor, Cbor)> = Vec::with_capacity(argument.min(64) as usize);
                for _ in 0..argument {
                    let key = self.item(depth + 1)?;
                    if entries.iter().any(|(k, _)| *k == key) {
                        return Err(Rejected("Duplicate CBOR map key."));
                    }
                    let value = self.item(depth + 1)?;
                    entries.push((key, value));
                }
                Cbor::Map(entries)
            }
            7 => match info {
                20 => Cbor::Bool(false),
                21 => Cbor::Bool(true),
                22 => Cbor::Null,
                _ => return Err(Rejected("Unsupported CBOR simple value.")),
            },
            _ => return Err(Rejected("CBOR tags and floats are not allowed.")),
        })
    }
}

// ---------------------------------------------------------------------------
// Client data (the JSON the browser signs on our behalf)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(default, rename = "crossOrigin")]
    cross_origin: bool,
}

/// The challenge the browser signed over, so storage can find its record
/// before the full check in [`verify_registration`] / [`verify_assertion`].
pub fn client_data_challenge(json: &[u8]) -> Result<Vec<u8>, Rejected> {
    let data: ClientData = serde_json::from_slice(json).map_err(|_| Rejected("Malformed client data."))?;
    b64url_decode(&data.challenge)
}

fn check_client_data(rp: &RelyingParty, json: &[u8], kind: &str, challenge: &[u8]) -> Result<(), Rejected> {
    let data: ClientData = serde_json::from_slice(json).map_err(|_| Rejected("Malformed client data."))?;
    if data.kind != kind {
        return Err(Rejected("Unexpected ceremony type."));
    }
    if b64url_decode(&data.challenge)? != challenge {
        return Err(Rejected("The challenge does not match."));
    }
    if data.origin != rp.origin {
        return Err(Rejected("The request came from a different origin."));
    }
    if data.cross_origin {
        return Err(Rejected("Cross-origin requests are not allowed."));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Authenticator data
// ---------------------------------------------------------------------------

struct AuthenticatorData<'a> {
    flags: u8,
    sign_count: u32,
    /// `(credential id, COSE public key bytes)` when the AT flag is set.
    attested: Option<(&'a [u8], &'a [u8])>,
}

fn parse_authenticator_data<'a>(rp: &RelyingParty, bytes: &'a [u8]) -> Result<AuthenticatorData<'a>, Rejected> {
    if bytes.len() < 37 {
        return Err(Rejected("Authenticator data is too short."));
    }
    if bytes[..32] != rp.id_hash() {
        return Err(Rejected("The passkey belongs to a different site."));
    }
    let flags = bytes[32];
    let sign_count = u32::from_be_bytes(bytes[33..37].try_into().unwrap());
    let mut rest = &bytes[37..];
    let mut attested = None;
    if flags & FLAG_ATTESTED != 0 {
        if rest.len() < 18 {
            return Err(Rejected("Attested credential data is truncated."));
        }
        let id_len = usize::from(u16::from_be_bytes([rest[16], rest[17]]));
        rest = &rest[18..];
        if id_len == 0 || id_len > 1023 || rest.len() < id_len {
            return Err(Rejected("Invalid credential id length."));
        }
        let (credential_id, after_id) = rest.split_at(id_len);
        let (_, key_len) = Cbor::decode(after_id)?;
        attested = Some((credential_id, &after_id[..key_len]));
        rest = &after_id[key_len..];
    }
    if flags & FLAG_EXTENSIONS != 0 {
        let (_, used) = Cbor::decode(rest)?;
        rest = &rest[used..];
    }
    if !rest.is_empty() {
        return Err(Rejected("Trailing bytes in authenticator data."));
    }
    if flags & FLAG_USER_PRESENT == 0 {
        return Err(Rejected("User presence was not confirmed."));
    }
    if flags & FLAG_USER_VERIFIED == 0 {
        return Err(Rejected("User verification is required. Use a passkey protected by a PIN or biometrics."));
    }
    Ok(AuthenticatorData { flags, sign_count, attested })
}

// ---------------------------------------------------------------------------
// COSE keys and signatures
// ---------------------------------------------------------------------------

/// A parsed COSE_Key, ready for `ring`.
pub struct PublicKey {
    pub algorithm: i64,
    verification: &'static dyn ring::signature::VerificationAlgorithm,
    key: Vec<u8>,
}

impl PublicKey {
    /// Parse a COSE_Key (RFC 9053) and reject anything outside [`ALGORITHMS`].
    pub fn parse(cose: &[u8]) -> Result<Self, Rejected> {
        let key = Cbor::decode_exact(cose)?;
        let kty = key.get_int(1).ok_or(Rejected("COSE key has no type."))?;
        let algorithm = key.get_int(3).ok_or(Rejected("COSE key has no algorithm."))?;
        let algorithm = i64::try_from(algorithm).map_err(|_| Rejected("Unsupported algorithm."))?;
        match (kty, algorithm) {
            (2, ES256) => {
                if key.get_int(-1) != Some(1) {
                    return Err(Rejected("ES256 keys must use the P-256 curve."));
                }
                let x = key.get_bytes(-2).ok_or(Rejected("Malformed EC key."))?;
                let y = key.get_bytes(-3).ok_or(Rejected("Malformed EC key."))?;
                if x.len() != 32 || y.len() != 32 {
                    return Err(Rejected("Malformed EC key."));
                }
                let mut point = Vec::with_capacity(65);
                point.push(0x04);
                point.extend_from_slice(x);
                point.extend_from_slice(y);
                Ok(Self { algorithm, verification: &ring::signature::ECDSA_P256_SHA256_ASN1, key: point })
            }
            (3, RS256) => {
                let n = key.get_bytes(-1).ok_or(Rejected("Malformed RSA key."))?;
                let e = key.get_bytes(-2).ok_or(Rejected("Malformed RSA key."))?;
                if n.len() < 256 || n.len() > 1024 || e.is_empty() || e.len() > 4 {
                    return Err(Rejected("RSA keys must be between 2048 and 8192 bits."));
                }
                Ok(Self {
                    algorithm,
                    verification: &ring::signature::RSA_PKCS1_2048_8192_SHA256,
                    key: der_rsa_public_key(n, e),
                })
            }
            (1, EDDSA) => {
                if key.get_int(-1) != Some(6) {
                    return Err(Rejected("EdDSA keys must use Ed25519."));
                }
                let x = key.get_bytes(-2).ok_or(Rejected("Malformed Ed25519 key."))?;
                if x.len() != 32 {
                    return Err(Rejected("Malformed Ed25519 key."));
                }
                Ok(Self { algorithm, verification: &ring::signature::ED25519, key: x.to_vec() })
            }
            _ => Err(Rejected("Unsupported passkey algorithm.")),
        }
    }

    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), Rejected> {
        ring::signature::UnparsedPublicKey::new(self.verification, &self.key)
            .verify(message, signature)
            .map_err(|_| Rejected("The signature is invalid."))
    }
}

/// PKCS#1 `RSAPublicKey ::= SEQUENCE { modulus INTEGER, publicExponent INTEGER }`.
fn der_rsa_public_key(n: &[u8], e: &[u8]) -> Vec<u8> {
    fn integer(out: &mut Vec<u8>, value: &[u8]) {
        let value = &value[value.iter().position(|b| *b != 0).unwrap_or(value.len())..];
        let pad = value.first().is_none_or(|b| b & 0x80 != 0);
        out.push(0x02);
        length(out, value.len() + usize::from(pad));
        if pad {
            out.push(0);
        }
        out.extend_from_slice(value);
    }
    fn length(out: &mut Vec<u8>, len: usize) {
        if len < 0x80 {
            out.push(len as u8);
        } else {
            let bytes = len.to_be_bytes();
            let bytes = &bytes[bytes.iter().position(|b| *b != 0).unwrap()..];
            out.push(0x80 | bytes.len() as u8);
            out.extend_from_slice(bytes);
        }
    }
    let mut body = Vec::with_capacity(n.len() + e.len() + 16);
    integer(&mut body, n);
    integer(&mut body, e);
    let mut out = Vec::with_capacity(body.len() + 4);
    out.push(0x30);
    length(&mut out, body.len());
    out.extend_from_slice(&body);
    out
}

// ---------------------------------------------------------------------------
// Ceremonies
// ---------------------------------------------------------------------------

/// What a verified registration yields; this is what gets stored.
pub struct Registration {
    pub credential_id: Vec<u8>,
    /// The COSE_Key bytes, re-parsed with [`PublicKey::parse`] at sign-in.
    pub public_key: Vec<u8>,
    pub algorithm: i64,
    pub sign_count: u32,
    pub backup_eligible: bool,
    pub backed_up: bool,
}

/// Verify a `navigator.credentials.create()` result. `credential_id` is the
/// `rawId` the browser reported; it must match the attested data.
pub fn verify_registration(
    rp: &RelyingParty,
    challenge: &[u8],
    credential_id: &[u8],
    client_data_json: &[u8],
    attestation_object: &[u8],
) -> Result<Registration, Rejected> {
    check_client_data(rp, client_data_json, "webauthn.create", challenge)?;
    let object = Cbor::decode_exact(attestation_object)?;
    match object.get(&Cbor::Text("fmt".into())) {
        Some(Cbor::Text(_)) => {}
        _ => return Err(Rejected("Malformed attestation object.")),
    }
    let Some(Cbor::Bytes(auth_data)) = object.get(&Cbor::Text("authData".into())) else {
        return Err(Rejected("Malformed attestation object."));
    };
    let data = parse_authenticator_data(rp, auth_data)?;
    let (attested_id, cose) = data.attested.ok_or(Rejected("No credential was created."))?;
    if attested_id != credential_id {
        return Err(Rejected("The credential id does not match."));
    }
    let key = PublicKey::parse(cose)?;
    Ok(Registration {
        credential_id: attested_id.to_vec(),
        public_key: cose.to_vec(),
        algorithm: key.algorithm,
        sign_count: data.sign_count,
        backup_eligible: data.flags & FLAG_BACKUP_ELIGIBLE != 0,
        backed_up: data.flags & FLAG_BACKED_UP != 0,
    })
}

pub struct Assertion {
    pub sign_count: u32,
    pub backed_up: bool,
}

/// Verify a `navigator.credentials.get()` result against a stored passkey.
/// `stored_sign_count` detects cloned authenticators (WebAuthn §7.2 step 21).
pub fn verify_assertion(
    rp: &RelyingParty,
    challenge: &[u8],
    public_key: &[u8],
    stored_sign_count: u32,
    client_data_json: &[u8],
    authenticator_data: &[u8],
    signature: &[u8],
) -> Result<Assertion, Rejected> {
    check_client_data(rp, client_data_json, "webauthn.get", challenge)?;
    let data = parse_authenticator_data(rp, authenticator_data)?;
    let key = PublicKey::parse(public_key).map_err(|_| Rejected("The stored passkey is unreadable."))?;
    let mut message = Vec::with_capacity(authenticator_data.len() + 32);
    message.extend_from_slice(authenticator_data);
    message.extend_from_slice(&Sha256::digest(client_data_json));
    key.verify(&message, signature)?;
    if (data.sign_count != 0 || stored_sign_count != 0) && data.sign_count <= stored_sign_count {
        return Err(Rejected("This passkey may have been cloned. Remove it and register a new one."));
    }
    Ok(Assertion { sign_count: data.sign_count, backed_up: data.flags & FLAG_BACKED_UP != 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Algorithm, Authenticator};

    fn rp() -> RelyingParty {
        RelyingParty::from_site_url("https://example.com", "Example").unwrap()
    }

    #[test]
    fn relying_party_comes_from_the_site_url() {
        let rp = RelyingParty::from_site_url("http://localhost:3000/", "Example").unwrap();
        assert_eq!(
            (rp.id.as_str(), rp.origin.as_str(), rp.name.as_str()),
            ("localhost", "http://localhost:3000", "Example")
        );
        let rp = RelyingParty::from_site_url("https://Example.com/a&b/?x#y", "Example").unwrap();
        assert_eq!((rp.id.as_str(), rp.origin.as_str()), ("example.com", "https://example.com"));
        for bad in ["example.com", "ftp://example.com", "https://", "https:///x", "https://ex ample.com"] {
            assert!(RelyingParty::from_site_url(bad, "x").is_err(), "{bad}");
        }
    }

    #[test]
    fn base64url_accepts_padded_and_unpadded_input() {
        assert_eq!(b64url_decode("AQID").unwrap(), [1, 2, 3]);
        assert_eq!(b64url_decode("AQI=").unwrap(), [1, 2]);
        assert_eq!(b64url_encode(&[251, 255]), "-_8");
        assert!(b64url_decode("AQ+D").is_err());
    }

    #[test]
    fn cbor_decodes_the_webauthn_subset_and_rejects_the_rest() {
        let (value, used) = Cbor::decode(&[0xa2, 0x01, 0x02, 0x20, 0x43, 1, 2, 3, 0xff]).unwrap();
        assert_eq!(used, 8);
        assert_eq!(value, Cbor::Map(vec![(Cbor::Int(1), Cbor::Int(2)), (Cbor::Int(-1), Cbor::Bytes(vec![1, 2, 3]))]));
        assert_eq!(Cbor::decode_exact(&[0x63, b'f', b'm', b't']).unwrap(), Cbor::Text("fmt".into()));
        assert_eq!(Cbor::decode_exact(&[0x81, 0xf5]).unwrap(), Cbor::Array(vec![Cbor::Bool(true)]));
        assert_eq!(Cbor::decode_exact(&[0x19, 0x01, 0x00]).unwrap(), Cbor::Int(256));
        assert_eq!(
            Cbor::decode_exact(&[0x3b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).unwrap(),
            Cbor::Int(-(1i128 << 64))
        );
        for bad in [
            &[0x5f][..],                         // indefinite bytes
            &[0x9f, 0xff][..],                   // indefinite array
            &[0xc0, 0x00][..],                   // tag
            &[0xf9, 0x00, 0x00][..],             // float
            &[0x43, 1, 2][..],                   // truncated bytes
            &[0x1b, 1, 2, 3][..],                // truncated argument
            &[0x5a, 0xff, 0xff, 0xff, 0xff][..], // huge length
            &[0xa2, 0x01, 0x02, 0x01, 0x03][..], // duplicate key
            &[0x62, 0xff, 0xfe][..],             // invalid UTF-8
            &[0x01, 0x02][..],                   // trailing bytes
            &[0x9a, 0x00, 0x01, 0x00, 0x00][..], // too many items
        ] {
            assert!(Cbor::decode_exact(bad).is_err(), "{bad:x?}");
        }
        let nested: Vec<u8> = std::iter::repeat_n(0x81, 20).chain([0x00]).collect();
        assert!(Cbor::decode_exact(&nested).is_err());
    }

    #[test]
    fn registration_and_assertion_round_trip_for_every_algorithm() {
        let rp = rp();
        for algorithm in [Algorithm::Es256, Algorithm::Ed25519] {
            let mut authenticator = Authenticator::new(algorithm);
            let challenge = b"registration-challenge".to_vec();
            let created = authenticator.create(&rp.origin, &challenge, b"user");
            let registration = verify_registration(
                &rp,
                &challenge,
                &created.credential_id,
                &created.client_data_json,
                &created.attestation_object,
            )
            .unwrap();
            assert_eq!(registration.credential_id, authenticator.credential_id);
            assert_eq!(registration.algorithm, algorithm.cose());
            assert!(registration.backup_eligible && registration.backed_up);
            assert_eq!(registration.sign_count, 0);

            let challenge = b"assertion-challenge".to_vec();
            let got = authenticator.get(&rp.origin, &challenge);
            let assertion = verify_assertion(
                &rp,
                &challenge,
                &registration.public_key,
                0,
                &got.client_data_json,
                &got.authenticator_data,
                &got.signature,
            )
            .unwrap();
            assert_eq!(assertion.sign_count, 1);
            // A wrong challenge, origin or signature fails; so does a counter that went backwards.
            let wrong = |challenge: &[u8], count: u32, signature: &[u8]| {
                verify_assertion(
                    &rp,
                    challenge,
                    &registration.public_key,
                    count,
                    &got.client_data_json,
                    &got.authenticator_data,
                    signature,
                )
                .map(|_| ())
            };
            assert!(wrong(b"other", 0, &got.signature).is_err());
            assert!(wrong(&challenge, 1, &got.signature).is_err());
            let mut tampered = got.signature.clone();
            tampered[10] ^= 1;
            assert!(wrong(&challenge, 0, &tampered).is_err());
            let other_rp = RelyingParty::from_site_url("https://other.example", "Other").unwrap();
            assert!(
                verify_assertion(
                    &other_rp,
                    &challenge,
                    &registration.public_key,
                    0,
                    &got.client_data_json,
                    &got.authenticator_data,
                    &got.signature
                )
                .is_err()
            );
        }
    }

    #[test]
    fn registration_rejects_bad_client_data_flags_and_keys() {
        let rp = rp();
        let challenge = b"challenge".to_vec();
        let ok = Authenticator::new(Algorithm::Es256).create(&rp.origin, &challenge, b"user");
        let verify = |created: &crate::testing::Created, challenge: &[u8]| {
            verify_registration(
                &rp,
                challenge,
                &created.credential_id,
                &created.client_data_json,
                &created.attestation_object,
            )
            .map(|_| ())
        };
        assert!(verify(&ok, &challenge).is_ok());
        assert!(verify(&ok, b"other").is_err());
        assert!(
            verify(
                &Authenticator::new(Algorithm::Es256).create("https://evil.example", &challenge, b"user"),
                &challenge
            )
            .is_err()
        );

        let mut no_uv = Authenticator::new(Algorithm::Es256);
        no_uv.user_verified = false;
        assert_eq!(
            verify(&no_uv.create(&rp.origin, &challenge, b"user"), &challenge).unwrap_err().0,
            "User verification is required. Use a passkey protected by a PIN or biometrics."
        );

        let mut mismatched = ok.clone();
        mismatched.credential_id[0] ^= 1;
        assert!(verify(&mismatched, &challenge).is_err());

        let mut get_type = Authenticator::new(Algorithm::Es256);
        get_type.ceremony_type = Some("webauthn.get");
        assert!(verify(&get_type.create(&rp.origin, &challenge, b"user"), &challenge).is_err());

        let mut cross_origin = Authenticator::new(Algorithm::Es256);
        cross_origin.cross_origin = true;
        assert!(verify(&cross_origin.create(&rp.origin, &challenge, b"user"), &challenge).is_err());

        let mut wrong_alg = Authenticator::new(Algorithm::Es256);
        wrong_alg.claimed_algorithm = Some(-36);
        assert!(verify(&wrong_alg.create(&rp.origin, &challenge, b"user"), &challenge).is_err());

        let mut truncated = ok.clone();
        truncated.attestation_object.truncate(40);
        assert!(verify(&truncated, &challenge).is_err());
    }

    #[test]
    fn rs256_verifies_a_known_openssl_signature() {
        // Produced once with `openssl genpkey -algorithm RSA` and `openssl dgst -sha256 -sign`.
        let n = hex(
            "cbce60852d3fbe7091bc05d2883f721e396f091197e2c34e2ffa917d84fcafe8f7188e17a747f03a89b5cb199c6aabfe6759ea4d02ef8a59b868e7d22c757f550c450e6447a44fba2ce79796e8dacaacdb7bfd8e562a4751e9803ebab840f66698c6b527e34e58a9ecf83d4e016998cd86ae064b08c02608ab221d08ee2d0c340c4ed947b10d5453dcea9917a4493e8aed5f533d36b7a0365ccca6b36104177be57777e73492f0855ace123b680fc1a1c845b7cd232434d7ca223b8c3fd7dad21931ffa1a4c34b39994f4d04063fc5f52d4c4b0bd76f2f58958462ad8988a9f28f71b1513afdf755fd055ddd16558833619b418c9e75f579c96f5c61addb2771",
        );
        let signature = hex(
            "2eae83d68e0643abc547964027d2994b38ce437fa5084ff03d897e13865413bca25c473c940696da142e9e5562ae0979f9e06e351b0a78bcbfe46889f310b16e9a62c44e092690f8d5e8ea53648a7b079d928527bcef205cf590d72fe2fec7ac50a83798a6b56b74a626eaa509fa54da54b74ae23044ec8807ec2d8901da92ab8ec0a50088ab6a087c9934bd72390bb67e8383c8642e104dd4f06b3da9ec944b2e04017bad180b7f40ac0288fadb954bee11f7d4c4507ea40ba694ad036fbeb9feeb8ad9533503141a30794d6d1f5c191560b34a0a70863142d463d2954fa6d1d77c82b7970a9e134815d072173e044dc0bdd3121c8fd043ecb487515736b70f",
        );
        let cose = crate::testing::cose_rsa(&n, &[0x01, 0x00, 0x01]);
        let key = PublicKey::parse(&cose).unwrap();
        assert_eq!(key.algorithm, RS256);
        // Fixed signed bytes, encoded like the other fixture values so project
        // renaming cannot change the message while leaving its signature intact.
        let mut message = hex("736869707275737420776562617574686e207273323536206b6e6f776e20616e73776572");
        assert!(key.verify(&message, &signature).is_ok());
        message.push(b'!');
        assert!(key.verify(&message, &signature).is_err());
        // Keys below 2048 bits, or with a bogus exponent, are refused.
        assert!(PublicKey::parse(&crate::testing::cose_rsa(&n[..128], &[1, 0, 1])).is_err());
        assert!(PublicKey::parse(&crate::testing::cose_rsa(&n, &[])).is_err());
    }

    #[test]
    fn der_encodes_integers_with_sign_padding_and_long_lengths() {
        assert_eq!(der_rsa_public_key(&[0x00, 0x7f], &[0x80]), [0x30, 0x07, 0x02, 0x01, 0x7f, 0x02, 0x02, 0x00, 0x80]);
        let big = der_rsa_public_key(&[0xff; 256], &[1, 0, 1]);
        assert_eq!(&big[..4], &[0x30, 0x82, 0x01, 0x0a]);
        assert_eq!(&big[4..9], &[0x02, 0x82, 0x01, 0x01, 0x00]);
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
    }
    #[test]
    fn attested_data_rejects_invalid_lengths_and_missing_credentials() {
        let rp = rp();
        let header = |flags: u8| {
            let mut bytes = rp.id_hash().to_vec();
            bytes.push(flags);
            bytes.extend_from_slice(&0u32.to_be_bytes());
            bytes
        };
        let mut truncated = header(FLAG_USER_PRESENT | FLAG_USER_VERIFIED | FLAG_ATTESTED);
        truncated.extend_from_slice(&[0; 17]);
        assert_eq!(
            parse_authenticator_data(&rp, &truncated).err().unwrap().0,
            "Attested credential data is truncated."
        );
        for (length, available) in [(0u16, 0usize), (1024, 1024), (32, 31)] {
            let mut bytes = header(FLAG_USER_PRESENT | FLAG_USER_VERIFIED | FLAG_ATTESTED);
            bytes.extend_from_slice(&[0; 16]);
            bytes.extend_from_slice(&length.to_be_bytes());
            bytes.extend(std::iter::repeat_n(1, available));
            assert_eq!(parse_authenticator_data(&rp, &bytes).err().unwrap().0, "Invalid credential id length.");
        }
        let challenge = b"challenge";
        let client = serde_json::to_vec(
            &serde_json::json!({"type":"webauthn.create", "origin":rp.origin, "challenge":b64url_encode(challenge)}),
        )
        .unwrap();
        let data = header(FLAG_USER_PRESENT | FLAG_USER_VERIFIED);
        // {fmt: none, authData: <37 bytes>}, a structurally valid object with no new credential.
        let mut object = hex("a263666d74646e6f6e656861757468446174615825");
        object.extend_from_slice(&data);
        assert_eq!(
            verify_registration(&rp, challenge, b"credential", &client, &object).err().unwrap().0,
            "No credential was created."
        );
        for malformed in [hex("a163666d7401"), hex("a163666d74646e6f6e65")] {
            assert_eq!(
                verify_registration(&rp, challenge, b"credential", &client, &malformed).err().unwrap().0,
                "Malformed attestation object."
            );
        }
        let mut extensions = header(FLAG_USER_PRESENT | FLAG_USER_VERIFIED | FLAG_EXTENSIONS);
        extensions.push(0xa0); // Empty extension map is valid, but it must consume all remaining bytes.
        assert_eq!(parse_authenticator_data(&rp, &extensions).unwrap().sign_count, 0);
        extensions.push(0);
        assert_eq!(
            parse_authenticator_data(&rp, &extensions).err().unwrap().0,
            "Trailing bytes in authenticator data."
        );
        extensions.truncate(37);
        assert_eq!(parse_authenticator_data(&rp, &extensions).err().unwrap().0, "Truncated CBOR.");
    }

    #[test]
    fn cose_keys_require_supported_curves_and_complete_key_material() {
        fn ec(curve: u8, x_len: u8, y_len: u8) -> Vec<u8> {
            let mut bytes = vec![0xa5, 1, 2, 3, 0x26, 0x20, curve, 0x21, 0x58, x_len];
            bytes.extend(std::iter::repeat_n(1, usize::from(x_len)));
            bytes.extend_from_slice(&[0x22, 0x58, y_len]);
            bytes.extend(std::iter::repeat_n(2, usize::from(y_len)));
            bytes
        }
        assert_eq!(PublicKey::parse(&ec(1, 32, 32)).unwrap().algorithm, ES256);
        assert_eq!(PublicKey::parse(&ec(2, 32, 32)).err().unwrap().0, "ES256 keys must use the P-256 curve.");
        for (x, y) in [(31, 32), (32, 31), (33, 32), (32, 33)] {
            assert_eq!(PublicKey::parse(&ec(1, x, y)).err().unwrap().0, "Malformed EC key.");
        }
        for (bytes, expected) in [
            (hex("a0"), "COSE key has no type."),
            (hex("a10102"), "COSE key has no algorithm."),
            (hex("a3010203262001"), "Malformed EC key."),
            (hex("a4010203262001214100"), "Malformed EC key."),
            (hex("a3010103272001"), "EdDSA keys must use Ed25519."),
            (hex("a3010103272006"), "Malformed Ed25519 key."),
            (hex("a4010103272006214100"), "Malformed Ed25519 key."),
        ] {
            assert_eq!(PublicKey::parse(&bytes).err().unwrap().0, expected);
        }
        let modulus = vec![0x81; 1024];
        assert_eq!(PublicKey::parse(&crate::testing::cose_rsa(&modulus, &[1, 0, 1])).unwrap().algorithm, RS256);
        for (n, e) in [(vec![0x81; 1025], vec![1, 0, 1]), (modulus, vec![1; 5])] {
            assert_eq!(
                PublicKey::parse(&crate::testing::cose_rsa(&n, &e)).err().unwrap().0,
                "RSA keys must be between 2048 and 8192 bits."
            );
        }
        let mut authenticator = Authenticator::new(Algorithm::Es256);
        let assertion = authenticator.get(&rp().origin, b"challenge");
        assert_eq!(
            verify_assertion(
                &rp(),
                b"challenge",
                &[0xa0],
                0,
                &assertion.client_data_json,
                &assertion.authenticator_data,
                &assertion.signature
            )
            .err()
            .unwrap()
            .0,
            "The stored passkey is unreadable."
        );
    }
}
