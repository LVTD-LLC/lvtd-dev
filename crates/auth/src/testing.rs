//! A software authenticator for tests: behaves like a browser plus a
//! platform authenticator, with knobs to produce invalid responses.
//! Enabled by the `test-support` feature (apps add it in dev-dependencies).

use crate::webauthn::{EDDSA, ES256, b64url_decode, b64url_encode};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, Ed25519KeyPair, KeyPair};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algorithm {
    Es256,
    Ed25519,
}

impl Algorithm {
    pub fn cose(self) -> i64 {
        match self {
            Self::Es256 => ES256,
            Self::Ed25519 => EDDSA,
        }
    }
}

enum Key {
    Es256(EcdsaKeyPair),
    Ed25519(Ed25519KeyPair),
}

pub struct Authenticator {
    pub algorithm: Algorithm,
    pub credential_id: Vec<u8>,
    pub sign_count: u32,
    pub user_verified: bool,
    pub backed_up: bool,
    pub cross_origin: bool,
    /// Override the `type` written into client data.
    pub ceremony_type: Option<&'static str>,
    /// Override the algorithm written into the COSE key (the key bytes stay the same).
    pub claimed_algorithm: Option<i64>,
    /// Override the relying party id hashed into authenticator data.
    pub rp_id: Option<String>,
    key: Key,
    rng: SystemRandom,
}

#[derive(Clone, Debug)]
pub struct Created {
    pub credential_id: Vec<u8>,
    pub client_data_json: Vec<u8>,
    pub attestation_object: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Got {
    pub credential_id: Vec<u8>,
    pub client_data_json: Vec<u8>,
    pub authenticator_data: Vec<u8>,
    pub signature: Vec<u8>,
    pub user_handle: Vec<u8>,
}

impl Authenticator {
    pub fn new(algorithm: Algorithm) -> Self {
        let rng = SystemRandom::new();
        let key = match algorithm {
            Algorithm::Es256 => {
                let document = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
                Key::Es256(EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, document.as_ref(), &rng).unwrap())
            }
            Algorithm::Ed25519 => {
                let document = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
                Key::Ed25519(Ed25519KeyPair::from_pkcs8(document.as_ref()).unwrap())
            }
        };
        let mut credential_id = vec![0u8; 32];
        getrandom::fill(&mut credential_id).unwrap();
        Self {
            algorithm,
            credential_id,
            sign_count: 0,
            user_verified: true,
            backed_up: true,
            cross_origin: false,
            ceremony_type: None,
            claimed_algorithm: None,
            rp_id: None,
            key,
            rng,
        }
    }

    fn rp_id_hash(&self, origin: &str) -> [u8; 32] {
        let host = self.rp_id.clone().unwrap_or_else(|| {
            let authority = origin.split("://").nth(1).unwrap_or(origin);
            authority.rsplit_once(':').map_or(authority, |(host, _)| host).to_string()
        });
        Sha256::digest(host.as_bytes()).into()
    }

    fn flags(&self, attested: bool) -> u8 {
        0x01 | if self.user_verified { 0x04 } else { 0 }
            | 0x08
            | if self.backed_up { 0x10 } else { 0 }
            | if attested { 0x40 } else { 0 }
    }

    fn client_data(&self, kind: &str, origin: &str, challenge: &[u8]) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "type": self.ceremony_type.unwrap_or(kind),
            "challenge": b64url_encode(challenge),
            "origin": origin,
            "crossOrigin": self.cross_origin,
        }))
        .unwrap()
    }

    fn cose_key(&self) -> Vec<u8> {
        let algorithm = self.claimed_algorithm.unwrap_or(self.algorithm.cose());
        match &self.key {
            Key::Es256(pair) => {
                let point = pair.public_key().as_ref();
                let mut out = vec![0xa5];
                cbor_int(&mut out, 1);
                cbor_int(&mut out, 2);
                cbor_int(&mut out, 3);
                cbor_int(&mut out, i128::from(algorithm));
                cbor_int(&mut out, -1);
                cbor_int(&mut out, 1);
                cbor_int(&mut out, -2);
                cbor_bytes(&mut out, &point[1..33]);
                cbor_int(&mut out, -3);
                cbor_bytes(&mut out, &point[33..65]);
                out
            }
            Key::Ed25519(pair) => {
                let mut out = vec![0xa4];
                cbor_int(&mut out, 1);
                cbor_int(&mut out, 1);
                cbor_int(&mut out, 3);
                cbor_int(&mut out, i128::from(algorithm));
                cbor_int(&mut out, -1);
                cbor_int(&mut out, 6);
                cbor_int(&mut out, -2);
                cbor_bytes(&mut out, pair.public_key().as_ref());
                out
            }
        }
    }

    fn sign(&self, message: &[u8]) -> Vec<u8> {
        match &self.key {
            Key::Es256(pair) => pair.sign(&self.rng, message).unwrap().as_ref().to_vec(),
            Key::Ed25519(pair) => pair.sign(message).as_ref().to_vec(),
        }
    }

    /// `navigator.credentials.create()` with `attestation: "none"`.
    pub fn create(&mut self, origin: &str, challenge: &[u8], _user_handle: &[u8]) -> Created {
        let mut auth_data = self.rp_id_hash(origin).to_vec();
        auth_data.push(self.flags(true));
        auth_data.extend_from_slice(&self.sign_count.to_be_bytes());
        auth_data.extend_from_slice(&[0u8; 16]); // AAGUID
        auth_data.extend_from_slice(&(self.credential_id.len() as u16).to_be_bytes());
        auth_data.extend_from_slice(&self.credential_id);
        auth_data.extend_from_slice(&self.cose_key());
        let mut attestation_object = vec![0xa3];
        cbor_text(&mut attestation_object, "fmt");
        cbor_text(&mut attestation_object, "none");
        cbor_text(&mut attestation_object, "attStmt");
        attestation_object.push(0xa0);
        cbor_text(&mut attestation_object, "authData");
        cbor_bytes(&mut attestation_object, &auth_data);
        Created {
            credential_id: self.credential_id.clone(),
            client_data_json: self.client_data("webauthn.create", origin, challenge),
            attestation_object,
        }
    }

    /// `navigator.credentials.get()`; increments the signature counter first.
    pub fn get(&mut self, origin: &str, challenge: &[u8]) -> Got {
        self.sign_count += 1;
        self.get_with_count(origin, challenge, self.sign_count)
    }

    pub fn get_with_count(&self, origin: &str, challenge: &[u8], sign_count: u32) -> Got {
        let mut authenticator_data = self.rp_id_hash(origin).to_vec();
        authenticator_data.push(self.flags(false));
        authenticator_data.extend_from_slice(&sign_count.to_be_bytes());
        let client_data_json = self.client_data("webauthn.get", origin, challenge);
        let mut message = authenticator_data.clone();
        message.extend_from_slice(&Sha256::digest(&client_data_json));
        Got {
            credential_id: self.credential_id.clone(),
            signature: self.sign(&message),
            client_data_json,
            authenticator_data,
            user_handle: Vec::new(),
        }
    }

    /// The JSON body a browser would POST after `create()`, from the options
    /// JSON the server produced (`{"publicKey": {...}}`).
    pub fn create_json(&mut self, origin: &str, options: &Value, name: &str) -> Value {
        let public_key = &options["publicKey"];
        let challenge = b64url_decode(public_key["challenge"].as_str().expect("challenge")).unwrap();
        let user_handle = b64url_decode(public_key["user"]["id"].as_str().expect("user id")).unwrap();
        if self.rp_id.is_none() {
            self.rp_id = public_key["rp"]["id"].as_str().map(str::to_string);
        }
        let created = self.create(origin, &challenge, &user_handle);
        json!({
            "name": name,
            "id": b64url_encode(&created.credential_id),
            "rawId": b64url_encode(&created.credential_id),
            "type": "public-key",
            "authenticatorAttachment": "platform",
            "response": {
                "clientDataJSON": b64url_encode(&created.client_data_json),
                "attestationObject": b64url_encode(&created.attestation_object),
                "transports": ["internal", "hybrid"],
            }
        })
    }

    /// The JSON body a browser would POST after `get()`.
    pub fn get_json(&mut self, origin: &str, options: &Value, user_handle: &[u8]) -> Value {
        let public_key = &options["publicKey"];
        let challenge = b64url_decode(public_key["challenge"].as_str().expect("challenge")).unwrap();
        if self.rp_id.is_none() {
            self.rp_id = public_key["rpId"].as_str().map(str::to_string);
        }
        let got = self.get(origin, &challenge);
        json!({
            "id": b64url_encode(&got.credential_id),
            "rawId": b64url_encode(&got.credential_id),
            "type": "public-key",
            "response": {
                "clientDataJSON": b64url_encode(&got.client_data_json),
                "authenticatorData": b64url_encode(&got.authenticator_data),
                "signature": b64url_encode(&got.signature),
                "userHandle": if user_handle.is_empty() { Value::Null } else { Value::String(b64url_encode(user_handle)) },
            }
        })
    }
}

/// A COSE RSA key, for verifying static RS256 vectors.
pub fn cose_rsa(n: &[u8], e: &[u8]) -> Vec<u8> {
    let mut out = vec![0xa4];
    cbor_int(&mut out, 1);
    cbor_int(&mut out, 3);
    cbor_int(&mut out, 3);
    cbor_int(&mut out, i128::from(crate::webauthn::RS256));
    cbor_int(&mut out, -1);
    cbor_bytes(&mut out, n);
    cbor_int(&mut out, -2);
    cbor_bytes(&mut out, e);
    out
}

fn cbor_head(out: &mut Vec<u8>, major: u8, argument: u64) {
    let major = major << 5;
    if argument < 24 {
        out.push(major | argument as u8);
    } else if argument <= 0xff {
        out.extend_from_slice(&[major | 24, argument as u8]);
    } else if argument <= 0xffff {
        out.push(major | 25);
        out.extend_from_slice(&(argument as u16).to_be_bytes());
    } else {
        out.push(major | 26);
        out.extend_from_slice(&(argument as u32).to_be_bytes());
    }
}

fn cbor_int(out: &mut Vec<u8>, value: i128) {
    if value >= 0 {
        cbor_head(out, 0, value as u64);
    } else {
        cbor_head(out, 1, (-1 - value) as u64);
    }
}

fn cbor_bytes(out: &mut Vec<u8>, value: &[u8]) {
    cbor_head(out, 2, value.len() as u64);
    out.extend_from_slice(value);
}

fn cbor_text(out: &mut Vec<u8>, value: &str) {
    cbor_head(out, 3, value.len() as u64);
    out.extend_from_slice(value.as_bytes());
}
