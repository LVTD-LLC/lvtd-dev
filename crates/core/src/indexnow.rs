//! Durable, host-independent IndexNow notifications. No work on normal page requests.
use anyhow::{Context, Result, ensure};
use axum::{Router, response::IntoResponse, routing::get};
use reqwest::{Client, Url};
use serde::Serialize;
use std::{collections::BTreeMap, time::Duration};
use tokio::task::JoinHandle;

use crate::Pool;

type Snapshot = BTreeMap<String, String>;
const ENDPOINT: &str = "https://api.indexnow.org/indexnow";

pub struct Config {
    pub enabled: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let enabled = match std::env::var("INDEXNOW_ENABLED").as_deref() {
            Err(_) | Ok("") | Ok("true") => true,
            Ok("false") => false,
            _ => anyhow::bail!("INDEXNOW_ENABLED must be true or false"),
        };
        Ok(Self { enabled })
    }
}

fn public_origin(site_url: &str) -> Result<Option<Url>> {
    let url = Url::parse(site_url).context("invalid IndexNow SITE_URL")?;
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "IndexNow SITE_URL must be an origin"
    );
    let public = match url.host_str() {
        Some(host) => {
            host.contains('.')
                && host.parse::<std::net::IpAddr>().is_err()
                && ![".localhost", ".local", ".internal", ".test", ".invalid", ".example"]
                    .iter()
                    .any(|s| host.ends_with(s))
                && !["example.com", "example.org", "example.net"].contains(&host)
        }
        _ => false,
    };
    Ok((url.scheme() == "https" && public).then_some(url))
}

fn snapshot(origin: &Url, paths: Vec<String>, revision: &str) -> Result<Snapshot> {
    paths
        .into_iter()
        .map(|path| {
            ensure!(path.starts_with('/') && !path.starts_with("//"), "IndexNow needs absolute local paths");
            let url = origin.join(&path)?;
            ensure!(
                url.origin() == origin.origin() && url.query().is_none() && url.fragment().is_none(),
                "IndexNow paths must stay on the canonical origin without queries or fragments"
            );
            Ok((url.to_string(), revision.to_owned()))
        })
        .collect()
}

fn changed(previous: &Snapshot, current: &Snapshot) -> Vec<String> {
    let mut urls: Vec<_> = current
        .iter()
        .filter(|(url, version)| previous.get(*url) != Some(*version))
        .map(|(url, _)| url.clone())
        .chain(previous.keys().filter(|url| !current.contains_key(*url)).cloned())
        .collect();
    urls.sort();
    urls.dedup();
    urls
}

struct Worker {
    pool: Pool,
    client: Client,
    origin: Url,
    key: String,
    revision: String,
    current: Snapshot,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Submission<'a> {
    host: &'a str,
    key: &'a str,
    key_location: String,
    url_list: &'a [String],
}

/// Call after migrations and before serving. The key is stable across restarts/replicas.
/// Network failures occur only in the background; database setup failures are startup errors.
pub async fn attach(
    app: Router,
    pool: Pool,
    site_url: &str,
    paths: Vec<String>,
    revision: &str,
    config: Config,
) -> Result<(Router, Option<JoinHandle<()>>)> {
    if !config.enabled {
        return Ok((app, None));
    }
    let Some(origin) = public_origin(site_url)? else {
        return Ok((app, None));
    };
    let worker = Worker::new(pool, origin, paths, revision).await?;
    let app = app.merge(worker.key_router());
    tracing::info!("IndexNow enabled for public pages");
    let task = tokio::spawn(async move {
        // Allow the listener, proxy and deployment health checks to settle first.
        let mut delay = 15;
        loop {
            tokio::time::sleep(Duration::from_secs(delay)).await;
            match worker.submit(ENDPOINT).await {
                Ok(count) => {
                    if count > 0 {
                        tracing::info!(count, "IndexNow URLs accepted (not proof of indexing)");
                    }
                    delay = 900;
                }
                Err(error) => {
                    // Never include response bodies, keys or URL credentials in logs.
                    tracing::warn!(error = %error, "IndexNow will retry; website remains available");
                    delay = (delay * 2).clamp(60, 3600);
                }
            }
        }
    });
    Ok((app, Some(task)))
}

impl Worker {
    async fn new(pool: Pool, origin: Url, paths: Vec<String>, revision: &str) -> Result<Self> {
        let current = snapshot(&origin, paths, revision)?;
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|_| anyhow::anyhow!("IndexNow random key generation failed"))?;
        let candidate: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let origin_str = origin.as_str();
        // A no-op upsert makes concurrent first boots converge on the same key.
        let row = sqlx::query!(
            "INSERT INTO indexnow_state (origin, key) VALUES ($1, $2)
            ON CONFLICT (origin) DO UPDATE SET origin = EXCLUDED.origin RETURNING key",
            origin_str,
            candidate
        )
        .fetch_one(&pool)
        .await?;
        let client =
            Client::builder().timeout(Duration::from_secs(15)).redirect(reqwest::redirect::Policy::none()).build()?;
        Ok(Self { pool, client, origin, key: row.key, revision: revision.into(), current })
    }

    fn key_router(&self) -> Router {
        let key = self.key.clone();
        let revision = self.revision.clone();
        Router::new().route(
            &format!("/{}.txt", self.key),
            get(move || {
                let key = key.clone();
                let revision = revision.clone();
                async move {
                    (
                        [
                            ("content-type", "text/plain; charset=utf-8"),
                            ("cache-control", "no-store"),
                            ("x-indexnow-revision", revision.as_str()),
                        ],
                        key,
                    )
                        .into_response()
                }
            }),
        )
    }

    async fn submit(&self, endpoint: &str) -> Result<usize> {
        let mut tx = self.pool.begin().await?;
        match self.submit_locked(&mut tx, endpoint).await {
            Ok(count) => {
                tx.commit().await?;
                Ok(count)
            }
            Err(error) => {
                // Dropping a SQLx transaction only queues rollback. Await it so a
                // prompt retry cannot mistake our previous lock for another replica.
                tx.rollback().await.context("IndexNow rollback failed")?;
                Err(error)
            }
        }
    }

    async fn submit_locked(&self, tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, endpoint: &str) -> Result<usize> {
        let origin = self.origin.as_str();
        // Only one replica submits a given origin at a time; other replicas do not wait.
        let Some(row) = sqlx::query!(
            "SELECT snapshot FROM indexnow_state WHERE origin = $1
            FOR UPDATE SKIP LOCKED",
            origin
        )
        .fetch_optional(&mut **tx)
        .await?
        else {
            return Ok(0);
        };
        let previous: Snapshot = serde_json::from_str(&row.snapshot)?;
        let urls = changed(&previous, &self.current);
        if urls.is_empty() {
            return Ok(0);
        }
        // A previous public URL can be deleted, but must still belong to this exact origin.
        for url in &urls {
            let parsed = Url::parse(url)?;
            ensure!(
                parsed.origin() == self.origin.origin() && parsed.query().is_none() && parsed.fragment().is_none(),
                "IndexNow stored URL is outside this origin"
            );
        }
        let key_location = self.origin.join(&format!("{}.txt", self.key))?.to_string();
        let response = self
            .client
            .get(&key_location)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("IndexNow ownership URL unavailable"))?;
        ensure!(response.status() == 200, "IndexNow ownership URL not ready");
        ensure!(
            response.headers().get("x-indexnow-revision").and_then(|h| h.to_str().ok()) == Some(&self.revision),
            "IndexNow public deployment differs; skipping old/preview replica"
        );
        ensure!(
            response.text().await.map_err(|_| anyhow::anyhow!("IndexNow ownership read failed"))? == self.key,
            "IndexNow ownership file does not match"
        );
        for batch in urls.chunks(10_000) {
            let response = self
                .client
                .post(endpoint)
                .json(&Submission {
                    host: self.origin.host_str().context("IndexNow host missing")?,
                    key: &self.key,
                    key_location: key_location.clone(),
                    url_list: batch,
                })
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("IndexNow submission transport failed"))?;
            ensure!(
                [200, 202].contains(&response.status().as_u16()),
                "IndexNow returned HTTP {}",
                response.status().as_u16()
            );
        }
        // Only advance after every batch is accepted. Failures retain removals for retry.
        let saved = serde_json::to_string(&self.current)?;
        sqlx::query!("UPDATE indexnow_state SET snapshot = $2 WHERE origin = $1", origin, saved)
            .execute(&mut **tx)
            .await?;
        Ok(urls.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Bytes, http::StatusCode, routing::post};
    use std::sync::{Arc, Mutex};

    #[test]
    fn deltas_include_changes_and_removals_but_not_unchanged_pages() {
        let old =
            BTreeMap::from([("/same".into(), "a".into()), ("/edit".into(), "a".into()), ("/gone".into(), "a".into())]);
        let new =
            BTreeMap::from([("/same".into(), "a".into()), ("/edit".into(), "b".into()), ("/new".into(), "a".into())]);
        assert_eq!(changed(&old, &new), ["/edit", "/gone", "/new"]);
    }

    #[test]
    fn local_and_noncanonical_targets_are_not_submitted() {
        for origin in [
            "http://prod.org",
            "https://localhost",
            "https://127.0.0.1",
            "https://[::1]",
            "https://app.local",
            "https://example.com",
        ] {
            assert!(public_origin(origin).unwrap().is_none(), "{origin}");
        }
        assert!(public_origin("https://user:pass@prod.org").is_err());
        assert!(public_origin("https://prod.org/subpath").is_err());
        let origin = public_origin("https://prod.org").unwrap().unwrap();
        for path in ["//evil.org/", "https://evil.org", "/?token=secret", "/#fragment"] {
            assert!(snapshot(&origin, vec![path.into()], "v1").is_err());
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn ownership_retry_restart_deletion_and_revision_guard(pool: Pool) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let mut worker =
            Worker::new(pool.clone(), origin.clone(), vec!["/".into(), "/gone".into()], "v1").await.unwrap();
        let calls = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let status = Arc::new(Mutex::new(StatusCode::TOO_MANY_REQUESTS));
        let app = worker.key_router().route(
            "/submit",
            post({
                let calls = calls.clone();
                let status = status.clone();
                move |body: Bytes| {
                    let calls = calls.clone();
                    let status = status.clone();
                    async move {
                        calls.lock().unwrap().push(serde_json::from_slice(&body).unwrap());
                        *status.lock().unwrap()
                    }
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let endpoint = origin.join("submit").unwrap().to_string();
        assert!(worker.submit(&endpoint).await.is_err());
        *status.lock().unwrap() = StatusCode::ACCEPTED;
        assert_eq!(worker.submit(&endpoint).await.unwrap(), 2);
        assert_eq!(worker.submit(&endpoint).await.unwrap(), 0);
        let restarted =
            Worker::new(pool.clone(), origin.clone(), vec!["/".into(), "/gone".into()], "v1").await.unwrap();
        assert_eq!(restarted.key, worker.key);
        assert_eq!(restarted.submit(&endpoint).await.unwrap(), 0);
        worker.current.remove(origin.join("gone").unwrap().as_str());
        assert_eq!(worker.submit(&endpoint).await.unwrap(), 1);
        let submitted = calls.lock().unwrap().last().unwrap().clone();
        assert_eq!(submitted["urlList"], serde_json::json!([origin.join("gone").unwrap().as_str()]));
        for i in 0..10_001 {
            worker.current.insert(origin.join(&format!("page-{i}")).unwrap().to_string(), "v1".into());
        }
        assert_eq!(worker.submit(&endpoint).await.unwrap(), 10_001);
        assert_eq!(calls.lock().unwrap()[3]["urlList"].as_array().unwrap().len(), 10_000);
        assert_eq!(calls.lock().unwrap()[4]["urlList"].as_array().unwrap().len(), 1);
        worker.current.insert(origin.join("new").unwrap().to_string(), "v2".into());
        worker.revision = "v2".into();
        assert!(worker.submit(&endpoint).await.is_err());
        assert_eq!(calls.lock().unwrap().len(), 5);
        server.abort();
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn locked_replica_and_failed_batches_preserve_the_checkpoint_for_retry(pool: Pool) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let worker =
            Worker::new(pool.clone(), origin.clone(), (0..10_001).map(|n| format!("/page-{n}")).collect(), "v2")
                .await
                .unwrap();
        let previous = Snapshot::from([(origin.join("removed").unwrap().to_string(), "v1".into())]);
        let saved = serde_json::to_string(&previous).unwrap();
        let origin_str = origin.as_str();
        sqlx::query!("UPDATE indexnow_state SET snapshot = $2 WHERE origin = $1", origin_str, saved)
            .execute(&pool)
            .await
            .unwrap();
        let ownership = Arc::new(Mutex::new((StatusCode::OK, "v2".to_string(), worker.key.clone())));
        let calls = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let router = Router::new()
            .route(
                &format!("/{}.txt", worker.key),
                get({
                    let ownership = ownership.clone();
                    move || {
                        let (status, revision, body) = ownership.lock().unwrap().clone();
                        async move { (status, [("x-indexnow-revision", revision)], body) }
                    }
                }),
            )
            .route(
                "/submit",
                post({
                    let calls = calls.clone();
                    move |body: Bytes| {
                        let mut calls = calls.lock().unwrap();
                        calls.push(serde_json::from_slice(&body).unwrap());
                        let status =
                            if calls.len() == 2 { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::ACCEPTED };
                        async move { status }
                    }
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let endpoint = origin.join("submit").unwrap().to_string();
        let mut competing = pool.begin().await.unwrap();
        sqlx::query!("SELECT snapshot FROM indexnow_state WHERE origin = $1 FOR UPDATE SKIP LOCKED", origin_str)
            .fetch_one(&mut *competing)
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), worker.submit(&endpoint)).await.unwrap().unwrap(),
            0,
            "another replica's lock must not block serving this worker"
        );
        assert!(calls.lock().unwrap().is_empty());
        competing.rollback().await.unwrap();

        for (status, revision, body, expected) in [
            (StatusCode::SERVICE_UNAVAILABLE, "v2", worker.key.clone(), "ownership URL not ready"),
            (StatusCode::OK, "v1", worker.key.clone(), "public deployment differs"),
            (StatusCode::OK, "v2", "wrong-key".into(), "ownership file does not match"),
        ] {
            *ownership.lock().unwrap() = (status, revision.into(), body);
            let error = worker.submit(&endpoint).await.unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
            assert!(calls.lock().unwrap().is_empty(), "ownership rejection cannot notify the provider");
        }
        *ownership.lock().unwrap() = (StatusCode::OK, "v2".into(), worker.key.clone());
        let error = worker.submit(&endpoint).await.unwrap_err().to_string();
        assert_eq!(error, "IndexNow returned HTTP 503");
        let row =
            sqlx::query!("SELECT snapshot FROM indexnow_state WHERE origin = $1 FOR UPDATE SKIP LOCKED", origin_str)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.snapshot, saved, "accepting only the first batch must not advance the checkpoint");
        assert_eq!(worker.submit(&endpoint).await.unwrap(), 10_002, "retry includes the removed URL");
        assert_eq!(worker.submit(&endpoint).await.unwrap(), 0, "successful retry persists its checkpoint");
        let submitted = calls.lock().unwrap();
        assert_eq!(submitted.len(), 4);
        assert_eq!(submitted[0], submitted[2], "retry repeats the complete first batch");
        assert_eq!(submitted[1], submitted[3], "retry repeats the failed last batch");
        assert_eq!(submitted[0]["urlList"].as_array().unwrap().len(), 10_000);
        assert_eq!(submitted[1]["urlList"].as_array().unwrap().len(), 2);
        let all: Vec<_> = submitted[..2].iter().flat_map(|batch| batch["urlList"].as_array().unwrap()).collect();
        assert!(all.iter().any(|url| url.as_str() == Some(origin.join("removed").unwrap().as_str())));
        server.abort();
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn corrupted_snapshot_cannot_submit_urls_outside_the_origin(pool: Pool) {
        let origin = Url::parse("http://127.0.0.1:1/").unwrap();
        let worker = Worker::new(pool.clone(), origin.clone(), vec!["/".into()], "v1").await.unwrap();
        let origin_str = origin.as_str();
        for url in [
            "https://another-domain.invalid/private",
            "http://127.0.0.1:1/?token=private",
            "http://127.0.0.1:1/#private",
        ] {
            let saved = serde_json::to_string(&Snapshot::from([(url.to_owned(), "v0".into())])).unwrap();
            sqlx::query!("UPDATE indexnow_state SET snapshot = $2 WHERE origin = $1", origin_str, saved)
                .execute(&pool)
                .await
                .unwrap();
            // Validation must fail before even attempting ownership or provider I/O.
            let error = worker.submit("invalid endpoint").await.unwrap_err().to_string();
            assert_eq!(error, "IndexNow stored URL is outside this origin");
            let row = sqlx::query!(
                "SELECT snapshot FROM indexnow_state WHERE origin = $1 FOR UPDATE SKIP LOCKED",
                origin_str
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(row.snapshot, saved, "rejection must not discard the pending notification");
        }
    }
}
