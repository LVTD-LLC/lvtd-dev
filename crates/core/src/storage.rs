//! Object storage for user uploads: an S3-compatible bucket (Cloudflare R2,
//! AWS S3, MinIO, Backblaze B2, …) when `STORAGE_S3_*` is configured, a
//! local directory otherwise. The app decides what to store and under which
//! key; this module only moves bytes and signs requests.
//!
//! The S3 client is Signature Version 4 over the `reqwest` client already in
//! the tree rather than an SDK: it needs only `PutObject`, `GetObject` and
//! `DeleteObject`, and every generated project would otherwise carry the
//! SDK's dependency tree. Objects are read and written whole; it is meant for
//! uploads of a few megabytes, not for streaming large files.

use anyhow::{Context, Result, bail, ensure};
use axum::body::Bytes;
use hmac::{Hmac, KeyInit, Mac};
use reqwest::{Client, Method, StatusCode, Url};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Duration;
use time::OffsetDateTime;
use time::macros::format_description;

/// Keys are `/`-separated segments of `[A-Za-z0-9._-]`, never `.` or `..`:
/// safe as object keys, URL paths and file paths alike.
pub const MAX_KEY_LEN: usize = 512;

pub fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_LEN
        && key.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

fn checked(key: &str) -> Result<&str> {
    ensure!(is_valid_key(key), "invalid storage key {key:?}");
    Ok(key)
}

/// The media type a key's extension implies; what the local backend serves.
/// S3 stores the type given at upload instead.
pub fn content_type_for(key: &str) -> &'static str {
    match key.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain; charset=utf-8",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

/// A stored object, read back whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Object {
    pub content_type: String,
    pub bytes: Bytes,
}

#[derive(Clone, Debug)]
pub struct S3Config {
    pub bucket: String,
    /// The service origin, e.g. `https://<account>.r2.cloudflarestorage.com`
    /// or `https://s3.eu-west-1.amazonaws.com`. Requests use path-style
    /// addressing (`<endpoint>/<bucket>/<key>`), which every provider accepts.
    pub endpoint: String,
    /// `auto` for R2; AWS needs the bucket's region.
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Where the bucket is served publicly, if anywhere (an R2 custom domain,
    /// a CDN). Objects are then linked there directly instead of through the app.
    pub public_url: Option<String>,
}

/// Where uploads live. Built once at startup and kept in the app state.
pub struct Storage {
    backend: Backend,
}

enum Backend {
    Local { dir: PathBuf },
    S3(S3),
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

impl Storage {
    /// `STORAGE_S3_BUCKET`, `STORAGE_S3_ENDPOINT`, `STORAGE_S3_ACCESS_KEY_ID`
    /// and `STORAGE_S3_SECRET_ACCESS_KEY` all set selects the bucket
    /// (`STORAGE_S3_REGION` defaults to `auto`, `STORAGE_S3_PUBLIC_URL` is
    /// optional). All blank keeps uploads under `STORAGE_LOCAL_DIR` (default
    /// `./storage`). Anything in between is a configuration error.
    pub fn from_env() -> Result<Self> {
        let bucket = var("STORAGE_S3_BUCKET");
        let endpoint = var("STORAGE_S3_ENDPOINT");
        let access_key_id = var("STORAGE_S3_ACCESS_KEY_ID");
        let secret_access_key = var("STORAGE_S3_SECRET_ACCESS_KEY");
        let public_url = var("STORAGE_S3_PUBLIC_URL");
        let required = [&bucket, &endpoint, &access_key_id, &secret_access_key];
        if required.iter().all(|value| value.is_none()) {
            ensure!(public_url.is_none(), "STORAGE_S3_PUBLIC_URL needs the STORAGE_S3_* bucket settings");
            return Self::local(var("STORAGE_LOCAL_DIR").unwrap_or_else(|| "./storage".into()));
        }
        let (Some(bucket), Some(endpoint), Some(access_key_id), Some(secret_access_key)) =
            (bucket, endpoint, access_key_id, secret_access_key)
        else {
            bail!(
                "set all of STORAGE_S3_BUCKET, STORAGE_S3_ENDPOINT, STORAGE_S3_ACCESS_KEY_ID and \
                 STORAGE_S3_SECRET_ACCESS_KEY to use a bucket, or none of them to store uploads on disk"
            );
        };
        Self::s3(S3Config {
            bucket,
            endpoint,
            region: var("STORAGE_S3_REGION").unwrap_or_else(|| "auto".into()),
            access_key_id,
            secret_access_key,
            public_url,
        })
    }

    /// Uploads as files under `dir`, created if missing. Fine for development
    /// and single-machine deployments with a persistent volume.
    pub fn local(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).with_context(|| format!("creating the upload directory {}", dir.display()))?;
        Ok(Self { backend: Backend::Local { dir } })
    }

    pub fn s3(config: S3Config) -> Result<Self> {
        Ok(Self { backend: Backend::S3(S3::new(config)?) })
    }

    /// One line for the startup log. Never includes credentials.
    pub fn describe(&self) -> String {
        match &self.backend {
            Backend::Local { dir } => format!("local directory {}", dir.display()),
            Backend::S3(s3) => format!("S3 bucket {} at {}", s3.cfg.bucket, s3.cfg.endpoint),
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self.backend, Backend::Local { .. })
    }

    /// The object's public URL when the bucket has one; `None` means the app
    /// has to serve it itself.
    pub fn public_url(&self, key: &str) -> Option<String> {
        match &self.backend {
            Backend::S3(S3 { cfg: S3Config { public_url: Some(base), .. }, .. }) if is_valid_key(key) => {
                Some(format!("{base}/{}", uri_encode(key)))
            }
            _ => None,
        }
    }

    /// Store `bytes` at `key`, replacing anything already there.
    pub async fn put(&self, key: &str, content_type: &str, bytes: Bytes) -> Result<()> {
        let key = checked(key)?;
        match &self.backend {
            Backend::Local { dir } => {
                let path = dir.join(key);
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                // Write beside the target and rename so a concurrent read never sees a partial file.
                let mut suffix = [0u8; 4];
                getrandom::fill(&mut suffix)?;
                let part = path.with_extension(format!("{}.part", hex(&suffix)));
                let result = async {
                    tokio::fs::write(&part, &bytes).await?;
                    tokio::fs::rename(&part, &path).await
                }
                .await;
                if result.is_err() {
                    // A failed write or rename must not retain the private upload.
                    // Preserve the original error even if cleanup also fails.
                    let _ = tokio::fs::remove_file(&part).await;
                }
                result.map_err(Into::into)
            }
            Backend::S3(s3) => {
                let response = s3.request(Method::PUT, key, Some(content_type), bytes).await?;
                s3.expect(response, "PutObject", &[StatusCode::OK]).await.map(drop)
            }
        }
    }

    /// `None` when nothing is stored at `key` (or the key is malformed).
    pub async fn get(&self, key: &str) -> Result<Option<Object>> {
        if !is_valid_key(key) {
            return Ok(None);
        }
        match &self.backend {
            Backend::Local { dir } => match tokio::fs::read(dir.join(key)).await {
                Ok(bytes) => Ok(Some(Object { content_type: content_type_for(key).into(), bytes: bytes.into() })),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.into()),
            },
            Backend::S3(s3) => {
                let response = s3.request(Method::GET, key, None, Bytes::new()).await?;
                if response.status() == StatusCode::NOT_FOUND {
                    return Ok(None);
                }
                let response = s3.expect(response, "GetObject", &[StatusCode::OK]).await?;
                let content_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("application/octet-stream")
                    .to_owned();
                Ok(Some(Object { content_type, bytes: response.bytes().await? }))
            }
        }
    }

    /// Remove `key`; already gone is not an error.
    pub async fn delete(&self, key: &str) -> Result<()> {
        let key = checked(key)?;
        match &self.backend {
            Backend::Local { dir } => match tokio::fs::remove_file(dir.join(key)).await {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            },
            Backend::S3(s3) => {
                let response = s3.request(Method::DELETE, key, None, Bytes::new()).await?;
                s3.expect(response, "DeleteObject", &[StatusCode::NO_CONTENT, StatusCode::OK, StatusCode::NOT_FOUND])
                    .await
                    .map(drop)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// S3: path-style requests signed with AWS Signature Version 4
// ---------------------------------------------------------------------------

struct S3 {
    cfg: S3Config,
    client: Client,
    /// The `Host` header reqwest will send, which the signature covers.
    host: String,
}

impl S3 {
    fn new(mut cfg: S3Config) -> Result<Self> {
        let url = Url::parse(&cfg.endpoint).context("STORAGE_S3_ENDPOINT must be a URL")?;
        let host = url
            .host_str()
            .filter(|_| matches!(url.scheme(), "http" | "https"))
            .with_context(|| format!("STORAGE_S3_ENDPOINT must be an http(s) origin, not {:?}", cfg.endpoint))?;
        ensure!(
            url.path() == "/" && url.query().is_none() && url.username().is_empty(),
            "STORAGE_S3_ENDPOINT must be an origin without a path, e.g. https://<account>.r2.cloudflarestorage.com"
        );
        ensure!(
            !cfg.bucket.is_empty()
                && cfg.bucket.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.')),
            "STORAGE_S3_BUCKET must be a bucket name (lowercase letters, digits, dashes and dots)"
        );
        ensure!(!cfg.region.is_empty() && !cfg.region.contains('/'), "STORAGE_S3_REGION is malformed");
        ensure!(!cfg.access_key_id.contains('/'), "STORAGE_S3_ACCESS_KEY_ID is malformed");
        let host = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        cfg.endpoint = cfg.endpoint.trim_end_matches('/').to_owned();
        if let Some(public_url) = &mut cfg.public_url {
            let parsed = Url::parse(public_url).context("STORAGE_S3_PUBLIC_URL must be a URL")?;
            ensure!(matches!(parsed.scheme(), "http" | "https"), "STORAGE_S3_PUBLIC_URL must be an http(s) URL");
            *public_url = public_url.trim_end_matches('/').to_owned();
        }
        let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
        Ok(Self { cfg, client, host })
    }

    async fn request(
        &self,
        method: Method,
        key: &str,
        content_type: Option<&str>,
        body: Bytes,
    ) -> Result<reqwest::Response> {
        let payload_hash = hex(&Sha256::digest(&body));
        let timestamp = OffsetDateTime::now_utc().format(AMZ_DATE)?;
        let mut headers = vec![
            ("host".to_owned(), self.host.clone()),
            ("x-amz-content-sha256".to_owned(), payload_hash.clone()),
            ("x-amz-date".to_owned(), timestamp.clone()),
        ];
        if let Some(content_type) = content_type {
            headers.push(("content-type".to_owned(), content_type.to_owned()));
        }
        headers.sort();
        let encoded = uri_encode(key);
        let authorization = authorization(
            &Credentials {
                access_key_id: &self.cfg.access_key_id,
                secret_access_key: &self.cfg.secret_access_key,
                region: &self.cfg.region,
            },
            method.as_str(),
            &format!("/{}/{encoded}", self.cfg.bucket),
            &headers,
            &payload_hash,
            &timestamp,
        );
        let mut request = self.client.request(method, format!("{}/{}/{encoded}", self.cfg.endpoint, self.cfg.bucket));
        for (name, value) in &headers {
            if name != "host" {
                request = request.header(name.as_str(), value.as_str());
            }
        }
        Ok(request.header(reqwest::header::AUTHORIZATION, authorization).body(body).send().await?)
    }

    /// Fail with the operation, status and the start of the provider's
    /// message (never the request, which carries credentials).
    async fn expect(
        &self,
        response: reqwest::Response,
        operation: &str,
        ok: &[StatusCode],
    ) -> Result<reqwest::Response> {
        let status = response.status();
        if ok.contains(&status) {
            return Ok(response);
        }
        let body = response.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(300).collect();
        bail!("S3 {operation} on bucket {} failed with {status}: {}", self.cfg.bucket, excerpt.trim());
    }
}

const AMZ_DATE: &[time::format_description::BorrowedFormatItem<'static>] =
    format_description!("[year][month][day]T[hour][minute][second]Z");
const ALGORITHM: &str = "AWS4-HMAC-SHA256";
const SERVICE: &str = "s3";

struct Credentials<'a> {
    access_key_id: &'a str,
    secret_access_key: &'a str,
    region: &'a str,
}

/// The `Authorization` header for one request. `headers` are the signed
/// headers as (lowercase name, value), sorted by name and including `host`.
fn authorization(
    credentials: &Credentials<'_>,
    method: &str,
    canonical_uri: &str,
    headers: &[(String, String)],
    payload_hash: &str,
    timestamp: &str,
) -> String {
    let date = &timestamp[..8];
    let signed_headers = headers.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join(";");
    let canonical_headers: String = headers.iter().map(|(name, value)| format!("{name}:{}\n", value.trim())).collect();
    // Method, URI, (empty) query string, headers, signed header names, payload hash.
    let canonical_request =
        format!("{method}\n{canonical_uri}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
    let scope = format!("{date}/{}/{SERVICE}/aws4_request", credentials.region);
    let string_to_sign =
        format!("{ALGORITHM}\n{timestamp}\n{scope}\n{}", hex(&Sha256::digest(canonical_request.as_bytes())));
    let key = hmac(format!("AWS4{}", credentials.secret_access_key).as_bytes(), date.as_bytes());
    let key = hmac(&key, credentials.region.as_bytes());
    let key = hmac(&key, SERVICE.as_bytes());
    let key = hmac(&key, b"aws4_request");
    let signature = hex(&hmac(&key, string_to_sign.as_bytes()));
    format!(
        "{ALGORITHM} Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        credentials.access_key_id
    )
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC supports any key size");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// AWS URI encoding of an object key: unreserved characters and `/` as-is,
/// everything else percent-encoded per byte, uppercase.
fn uri_encode(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for byte in key.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, header};
    use axum::routing::put;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    const EMPTY_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn keys_are_single_level_safe_paths() {
        for key in ["avatars/u/x.png", "a", "a.b-c_d/e", "A/B/C"] {
            assert!(is_valid_key(key), "{key}");
        }
        for key in ["", "/a", "a/", "a//b", "../a", "a/../b", "a/.", ".", "a b", "a?b", "ü", &"x".repeat(513)] {
            assert!(!is_valid_key(key), "{key:?}");
        }
        assert_eq!(uri_encode("a b/ü~.png"), "a%20b/%C3%BC~.png");
        assert_eq!(content_type_for("x/y.PNG"), "image/png");
        assert_eq!(content_type_for("x/y.jpeg"), "image/jpeg");
        assert_eq!(content_type_for("noext"), "application/octet-stream");
    }

    /// The GET example from the AWS "Signature Calculations for the
    /// Authorization Header" documentation, with its published signature.
    #[test]
    fn signature_matches_the_aws_published_example() {
        let headers = [
            ("host", "examplebucket.s3.amazonaws.com"),
            ("range", "bytes=0-9"),
            ("x-amz-content-sha256", EMPTY_HASH),
            ("x-amz-date", "20130524T000000Z"),
        ]
        .map(|(name, value)| (name.to_owned(), value.to_owned()));
        let credentials = Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE",
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            region: "us-east-1",
        };
        let value = authorization(&credentials, "GET", "/test.txt", &headers, EMPTY_HASH, "20130524T000000Z");
        assert_eq!(
            value,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn configuration_requires_all_or_nothing() {
        let endpoint = "https://account.r2.cloudflarestorage.com";
        let config = |endpoint: &str, bucket: &str| S3Config {
            bucket: bucket.into(),
            endpoint: endpoint.into(),
            region: "auto".into(),
            access_key_id: "key".into(),
            secret_access_key: "secret".into(),
            public_url: None,
        };
        assert!(Storage::s3(config(endpoint, "uploads")).is_ok());
        assert!(Storage::s3(config("https://account.r2.cloudflarestorage.com/path", "uploads")).is_err());
        assert!(Storage::s3(config("ftp://host", "uploads")).is_err());
        assert!(Storage::s3(config(endpoint, "Bad Bucket")).is_err());
        assert!(Storage::s3(config(endpoint, "")).is_err());
        let storage =
            Storage::s3(S3Config { public_url: Some("https://files.example.com/".into()), ..config(endpoint, "b") })
                .unwrap();
        assert_eq!(storage.public_url("avatars/x.png").as_deref(), Some("https://files.example.com/avatars/x.png"));
        assert_eq!(storage.public_url("../x"), None);
        assert_eq!(storage.describe(), "S3 bucket b at https://account.r2.cloudflarestorage.com");
        assert!(!storage.is_local());
    }

    #[tokio::test]
    async fn local_directory_round_trip_and_path_safety() {
        let dir = std::env::temp_dir().join(format!("storage-test-{}", uuid::Uuid::now_v7()));
        let storage = Storage::local(&dir).unwrap();
        assert!(storage.is_local());
        assert_eq!(storage.public_url("a/b.png"), None);
        assert_eq!(storage.get("a/b.png").await.unwrap(), None);
        storage.put("a/b.png", "image/png", Bytes::from_static(b"png")).await.unwrap();
        let object = storage.get("a/b.png").await.unwrap().unwrap();
        assert_eq!((object.content_type.as_str(), &object.bytes[..]), ("image/png", &b"png"[..]));
        assert_eq!(std::fs::read_dir(dir.join("a")).unwrap().count(), 1, "no leftover part files");
        storage.put("a/b.png", "image/png", Bytes::from_static(b"png2")).await.unwrap();
        assert_eq!(&storage.get("a/b.png").await.unwrap().unwrap().bytes[..], b"png2");
        for key in ["../escape", "a/../../escape", "/etc/passwd", ""] {
            assert!(storage.put(key, "text/plain", Bytes::new()).await.is_err(), "{key}");
            assert_eq!(storage.get(key).await.unwrap(), None, "{key}");
            assert!(storage.delete(key).await.is_err(), "{key}");
        }
        storage.delete("a/b.png").await.unwrap();
        storage.delete("a/b.png").await.unwrap();
        assert_eq!(storage.get("a/b.png").await.unwrap(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    type Objects = Arc<Mutex<HashMap<String, (String, Vec<u8>)>>>;
    const KEY_ID: &str = "AKIATEST";
    const SECRET: &str = "test-secret";

    /// Verify the request against the same signing rules, so a drift in
    /// header handling between client and signer fails here.
    fn verify(headers: &HeaderMap, method: &str, bucket: &str, key: &str, body: &[u8]) -> Result<(), StatusCode> {
        let mut signed: Vec<(String, String)> = ["host", "x-amz-content-sha256", "x-amz-date", "content-type"]
            .into_iter()
            .filter_map(|name| headers.get(name).map(|value| (name.to_owned(), value.to_str().unwrap().to_owned())))
            .collect();
        signed.sort();
        let timestamp = headers["x-amz-date"].to_str().unwrap().to_owned();
        if headers["x-amz-content-sha256"].to_str().unwrap() != hex(&Sha256::digest(body)) {
            return Err(StatusCode::BAD_REQUEST);
        }
        let expected = authorization(
            &Credentials { access_key_id: KEY_ID, secret_access_key: SECRET, region: "auto" },
            method,
            &format!("/{bucket}/{}", uri_encode(key)),
            &signed,
            &hex(&Sha256::digest(body)),
            &timestamp,
        );
        if headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok()) == Some(expected.as_str()) {
            Ok(())
        } else {
            Err(StatusCode::FORBIDDEN)
        }
    }

    async fn s3_double() -> (String, Objects) {
        let objects: Objects = Arc::default();
        let router = Router::new()
            .route(
                "/{bucket}/{*key}",
                put(
                    |State(objects): State<Objects>,
                     Path((bucket, key)): Path<(String, String)>,
                     headers: HeaderMap,
                     body: Bytes| async move {
                        verify(&headers, "PUT", &bucket, &key, &body)?;
                        let content_type = headers[header::CONTENT_TYPE].to_str().unwrap().to_owned();
                        objects.lock().unwrap().insert(format!("{bucket}/{key}"), (content_type, body.to_vec()));
                        Ok::<_, StatusCode>(StatusCode::OK)
                    },
                )
                .get(
                    |State(objects): State<Objects>,
                     Path((bucket, key)): Path<(String, String)>,
                     headers: HeaderMap| async move {
                        verify(&headers, "GET", &bucket, &key, b"")?;
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
                        verify(&headers, "DELETE", &bucket, &key, b"")?;
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

    #[tokio::test]
    async fn s3_requests_are_signed_and_round_trip() {
        let (endpoint, objects) = s3_double().await;
        let storage = Storage::s3(S3Config {
            bucket: "uploads".into(),
            endpoint: endpoint.clone(),
            region: "auto".into(),
            access_key_id: KEY_ID.into(),
            secret_access_key: SECRET.into(),
            public_url: None,
        })
        .unwrap();
        assert_eq!(storage.get("avatars/u/x.png").await.unwrap(), None);
        storage.put("avatars/u/x.png", "image/png", Bytes::from_static(b"\x89PNG")).await.unwrap();
        assert_eq!(objects.lock().unwrap()["uploads/avatars/u/x.png"], ("image/png".to_owned(), b"\x89PNG".to_vec()));
        let object = storage.get("avatars/u/x.png").await.unwrap().unwrap();
        assert_eq!((object.content_type.as_str(), &object.bytes[..]), ("image/png", &b"\x89PNG"[..]));
        storage.delete("avatars/u/x.png").await.unwrap();
        assert_eq!(storage.get("avatars/u/x.png").await.unwrap(), None);
        assert!(objects.lock().unwrap().is_empty());
        storage.delete("avatars/u/x.png").await.unwrap();

        // A wrong secret is rejected by the service and surfaces as an error, without the secret.
        let wrong = Storage::s3(S3Config {
            bucket: "uploads".into(),
            endpoint,
            region: "auto".into(),
            access_key_id: KEY_ID.into(),
            secret_access_key: "other".into(),
            public_url: None,
        })
        .unwrap();
        let error = wrong.put("avatars/u/x.png", "image/png", Bytes::new()).await.unwrap_err().to_string();
        assert!(error.contains("PutObject") && error.contains("403"), "{error}");
        assert!(!error.contains("other"));
    }

    #[tokio::test]
    async fn failed_local_upload_preserves_existing_data_and_removes_its_temporary_file() {
        let dir = std::env::temp_dir().join(format!("storage-failure-{}", uuid::Uuid::now_v7()));
        let storage = Storage::local(&dir).unwrap();
        // A directory where the destination file should be makes the final rename fail.
        std::fs::create_dir_all(dir.join("avatars/collision.png")).unwrap();
        std::fs::write(dir.join("avatars/collision.png/keep"), b"existing").unwrap();
        assert!(storage.get("avatars/collision.png").await.is_err(), "I/O failure is not a missing object");
        assert!(storage.delete("avatars/collision.png").await.is_err(), "failed deletion must be reported");
        let error =
            storage.put("avatars/collision.png", "image/png", Bytes::from_static(b"private upload")).await.unwrap_err();
        assert!(error.downcast_ref::<std::io::Error>().is_some(), "preserve the original I/O error");
        assert_eq!(std::fs::read(dir.join("avatars/collision.png/keep")).unwrap(), b"existing");
        let files: Vec<_> =
            std::fs::read_dir(dir.join("avatars")).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(files, ["collision.png"], "a failed upload must not leave private temporary data");
        std::fs::remove_dir_all(dir.join("avatars/collision.png")).unwrap();
        storage.put("avatars/collision.png", "image/png", Bytes::from_static(b"recovered")).await.unwrap();
        assert_eq!(&storage.get("avatars/collision.png").await.unwrap().unwrap().bytes[..], b"recovered");
        storage.delete("avatars/collision.png").await.unwrap();
        assert!(std::fs::read_dir(dir.join("avatars")).unwrap().next().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
