//! Optional vector search on [Qdrant](https://qdrant.tech). One collection for
//! the whole app; every point carries the id of the user who owns it and every
//! query filters on that id, so a user can only ever search, read or delete
//! their own vectors. Bring your own embeddings: this module stores and
//! searches `f32` vectors and never calls an embedding model.
//!
//! Qdrant is reached over its REST API with the `reqwest` client already in
//! the tree; no SDK or gRPC stack is added. Configuration is runtime-only
//! (`QDRANT_URL` and friends in `.env.example`); blank configuration keeps
//! the app runnable with `state.vectors` set to `None`.
use anyhow::{Context as _, ensure};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::time::Duration;
use uuid::Uuid;

/// Payload key naming the owning user, indexed as Qdrant's tenant field.
/// Set by [`Client::upsert`]; never trust a caller-supplied value.
pub const OWNER_KEY: &str = "user_id";
/// Largest `limit` a search accepts.
pub const MAX_LIMIT: usize = 1000;
/// Qdrant's own ceiling on vector dimensions.
const MAX_VECTOR_SIZE: u64 = 65_536;

/// How Qdrant compares vectors. Match it to your embedding model
/// (`Cosine` suits most text embeddings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Distance {
    Cosine,
    Euclid,
    Dot,
    Manhattan,
}
impl Distance {
    pub const ALL: [Distance; 4] = [Distance::Cosine, Distance::Euclid, Distance::Dot, Distance::Manhattan];
    pub fn as_str(self) -> &'static str {
        match self {
            Distance::Cosine => "Cosine",
            Distance::Euclid => "Euclid",
            Distance::Dot => "Dot",
            Distance::Manhattan => "Manhattan",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.as_str().eq_ignore_ascii_case(value))
    }
}

// No Debug: the API key must not appear in logs.
#[derive(Clone)]
pub struct Config {
    url: String,
    api_key: Option<String>,
    pub collection: String,
    pub vector_size: u64,
    pub distance: Distance,
}
impl Config {
    /// `None` when every `QDRANT_*` variable is blank; an error when only some are set.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let read = |name| std::env::var(name).unwrap_or_default().trim().to_owned();
        Self::parse(
            read("QDRANT_URL"),
            read("QDRANT_API_KEY"),
            read("QDRANT_COLLECTION"),
            read("QDRANT_VECTOR_SIZE"),
            read("QDRANT_DISTANCE"),
        )
    }
    pub fn parse(
        url: String,
        api_key: String,
        collection: String,
        vector_size: String,
        distance: String,
    ) -> anyhow::Result<Option<Self>> {
        if [&url, &api_key, &collection, &vector_size, &distance].iter().all(|v| v.is_empty()) {
            return Ok(None);
        }
        ensure!(!url.is_empty(), "QDRANT_URL is required when any other QDRANT_* setting is set");
        let parsed = reqwest::Url::parse(&url).context("QDRANT_URL must be an http(s) URL")?;
        ensure!(
            matches!(parsed.scheme(), "http" | "https")
                && parsed.host_str().is_some()
                && parsed.query().is_none()
                && parsed.fragment().is_none()
                && parsed.path().trim_end_matches('/').is_empty(),
            "QDRANT_URL must be an http(s) origin such as https://example.cloud.qdrant.io:6333"
        );
        let collection = if collection.is_empty() { "documents".to_owned() } else { collection };
        ensure!(
            collection.len() <= 255 && collection.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
            "QDRANT_COLLECTION may contain only letters, digits, '_' and '-'"
        );
        let vector_size: u64 = vector_size
            .parse()
            .ok()
            .filter(|size| (1..=MAX_VECTOR_SIZE).contains(size))
            .context("QDRANT_VECTOR_SIZE must be the dimension of your embeddings (1 to 65536)")?;
        let distance = if distance.is_empty() {
            Distance::Cosine
        } else {
            Distance::parse(&distance).context("QDRANT_DISTANCE must be Cosine, Euclid, Dot or Manhattan")?
        };
        Ok(Some(Self {
            url: url.trim_end_matches('/').to_owned(),
            api_key: (!api_key.is_empty()).then_some(api_key),
            collection,
            vector_size,
            distance,
        }))
    }
}

/// A vector to store, with the payload Qdrant returns alongside search hits.
/// Keep payloads small (titles, foreign keys into Postgres); the source
/// text belongs in your own tables.
#[derive(Clone, Debug)]
pub struct Point {
    pub id: Uuid,
    pub vector: Vec<f32>,
    pub payload: Map<String, Value>,
}
impl Point {
    pub fn new(vector: Vec<f32>, payload: Map<String, Value>) -> Self {
        Self { id: Uuid::now_v7(), vector, payload }
    }
}

/// One search result, best first.
#[derive(Clone, Debug, Deserialize)]
pub struct Hit {
    pub id: Uuid,
    pub score: f32,
    #[serde(default)]
    pub payload: Map<String, Value>,
}

#[derive(Deserialize)]
struct Envelope<T> {
    result: T,
}
#[derive(Deserialize)]
struct ErrorEnvelope {
    status: ErrorStatus,
}
#[derive(Deserialize)]
struct ErrorStatus {
    error: String,
}
#[derive(Deserialize)]
struct CollectionInfo {
    config: CollectionConfig,
    #[serde(default)]
    payload_schema: Map<String, Value>,
}
#[derive(Deserialize)]
struct CollectionConfig {
    params: CollectionParams,
}
#[derive(Deserialize)]
struct CollectionParams {
    vectors: Value,
}
#[derive(Deserialize)]
struct Points {
    points: Vec<Hit>,
}

#[derive(Clone)]
pub struct Client {
    pub cfg: Config,
    http: reqwest::Client,
    base: String,
}

impl Client {
    pub fn new(cfg: Config) -> anyhow::Result<Self> {
        let base = cfg.url.clone();
        Ok(Self {
            cfg,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base,
        })
    }
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let request = self.http.request(method, format!("{}{path}", self.base));
        match &self.cfg.api_key {
            Some(key) => request.header("api-key", key),
            None => request,
        }
    }
    fn collection_path(&self, suffix: &str) -> String {
        format!("/collections/{}{suffix}", self.cfg.collection)
    }
    /// Unwrap Qdrant's `{ "result": …, "status": "ok" }` envelope; surface its
    /// error text on failure (it names collections and fields, never keys).
    async fn decode<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> anyhow::Result<T> {
        let status = response.status();
        if !status.is_success() {
            let detail =
                response.json::<ErrorEnvelope>().await.map(|e| format!(": {}", e.status.error)).unwrap_or_default();
            anyhow::bail!("Qdrant returned HTTP {status}{detail}");
        }
        Ok(response.json::<Envelope<T>>().await.context("Qdrant returned an unexpected body")?.result)
    }
    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &Value,
    ) -> anyhow::Result<T> {
        Self::decode(self.request(method, path).json(body).send().await?).await
    }
    fn check_dimension(&self, vector: &[f32]) -> anyhow::Result<()> {
        ensure!(
            vector.len() as u64 == self.cfg.vector_size,
            "vector has {} dimensions; the {} collection stores {}",
            vector.len(),
            self.cfg.collection,
            self.cfg.vector_size
        );
        ensure!(vector.iter().all(|v| v.is_finite()), "vector contains NaN or infinite values");
        Ok(())
    }

    /// The configured collection, or `None` when Qdrant does not have it yet.
    async fn collection(&self) -> anyhow::Result<Option<CollectionInfo>> {
        let response = self.request(reqwest::Method::GET, &self.collection_path("")).send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(Self::decode(response).await?))
    }

    /// Run once at startup: reach Qdrant, create the collection when it is
    /// missing, refuse to start against one whose vectors have a different
    /// size or distance, and index the owner field so per-user filters stay
    /// fast as the collection grows.
    pub async fn ensure_collection(&self) -> anyhow::Result<()> {
        let info = match self.collection().await.context("Qdrant is not reachable")? {
            Some(info) => info,
            None => {
                let body =
                    json!({ "vectors": { "size": self.cfg.vector_size, "distance": self.cfg.distance.as_str() } });
                let response = self.request(reqwest::Method::PUT, &self.collection_path("")).json(&body).send().await?;
                // A web and a worker process starting together both try; the loser verifies the winner's collection.
                if response.status() != reqwest::StatusCode::CONFLICT {
                    let _: bool = Self::decode(response).await?;
                    tracing::info!(collection = %self.cfg.collection, size = self.cfg.vector_size, "created Qdrant collection");
                }
                self.collection().await?.context("Qdrant collection vanished after creation")?
            }
        };
        let vectors = &info.config.params.vectors;
        ensure!(
            vectors["size"].as_u64() == Some(self.cfg.vector_size)
                && vectors["distance"].as_str() == Some(self.cfg.distance.as_str()),
            "Qdrant collection {} stores {} vectors with {} distance, but QDRANT_VECTOR_SIZE/QDRANT_DISTANCE say {} and {}",
            self.cfg.collection,
            vectors["size"],
            vectors["distance"],
            self.cfg.vector_size,
            self.cfg.distance.as_str()
        );
        if !info.payload_schema.contains_key(OWNER_KEY) {
            let body = json!({ "field_name": OWNER_KEY, "field_schema": { "type": "keyword", "is_tenant": true } });
            let _: Value = self.send(reqwest::Method::PUT, &self.collection_path("/index"), &body).await?;
        }
        Ok(())
    }

    /// The filter condition that scopes a request to one user. Push it into
    /// your own `must` list when you build a custom query.
    pub fn owner_condition(owner: Uuid) -> Value {
        json!({ "key": OWNER_KEY, "match": { "value": owner.to_string() } })
    }

    /// Store or replace vectors for `owner`. The owner is written into every
    /// payload, overriding anything the caller put under [`OWNER_KEY`].
    /// Waits until Qdrant has applied the change, so a search right after
    /// sees the new points.
    pub async fn upsert(&self, owner: Uuid, points: Vec<Point>) -> anyhow::Result<()> {
        if points.is_empty() {
            return Ok(());
        }
        let mut body = Vec::with_capacity(points.len());
        for mut point in points {
            self.check_dimension(&point.vector)?;
            point.payload.insert(OWNER_KEY.into(), Value::String(owner.to_string()));
            body.push(json!({ "id": point.id, "vector": point.vector, "payload": point.payload }));
        }
        let _: Value = self
            .send(reqwest::Method::PUT, &self.collection_path("/points?wait=true"), &json!({ "points": body }))
            .await?;
        Ok(())
    }

    /// The `limit` vectors of `owner` nearest to `vector`, best first.
    pub async fn search(&self, owner: Uuid, vector: &[f32], limit: usize) -> anyhow::Result<Vec<Hit>> {
        self.search_filtered(owner, vector, limit, Vec::new()).await
    }

    /// [`search`](Self::search) with extra Qdrant `must` conditions on the
    /// payload (for example `{"key": "kind", "match": {"value": "note"}}`).
    /// The owner condition is always added; callers cannot widen the scope.
    pub async fn search_filtered(
        &self,
        owner: Uuid,
        vector: &[f32],
        limit: usize,
        mut must: Vec<Value>,
    ) -> anyhow::Result<Vec<Hit>> {
        self.check_dimension(vector)?;
        must.push(Self::owner_condition(owner));
        let body = json!({
            "query": vector,
            "limit": limit.clamp(1, MAX_LIMIT),
            "filter": { "must": must },
            "with_payload": true,
        });
        let found: Points = self.send(reqwest::Method::POST, &self.collection_path("/points/query"), &body).await?;
        Ok(found.points)
    }

    /// Delete `ids` that belong to `owner`; someone else's ids are ignored.
    pub async fn delete(&self, owner: Uuid, ids: &[Uuid]) -> anyhow::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.delete_matching(vec![Self::owner_condition(owner), json!({ "has_id": ids })]).await
    }

    /// Delete everything `owner` stored, for example when their account is
    /// deleted (`jobs::PurgeVectors`).
    pub async fn delete_all(&self, owner: Uuid) -> anyhow::Result<()> {
        self.delete_matching(vec![Self::owner_condition(owner)]).await
    }

    async fn delete_matching(&self, must: Vec<Value>) -> anyhow::Result<()> {
        let body = json!({ "filter": { "must": must } });
        let _: Value =
            self.send(reqwest::Method::POST, &self.collection_path("/points/delete?wait=true"), &body).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
