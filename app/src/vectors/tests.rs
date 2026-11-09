//! The Qdrant client against an in-process double of the REST endpoints it
//! uses. Nothing here contacts a real Qdrant; the double stores points in
//! memory and evaluates the same owner filters the client sends.
use super::*;
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const API_KEY: &str = "qdrant-fixture-key";

fn config() -> Config {
    Config::parse("http://unused".into(), API_KEY.into(), "notes".into(), "3".into(), "".into()).unwrap().unwrap()
}

#[derive(Clone)]
struct Stored {
    vector: Vec<f32>,
    payload: Map<String, Value>,
}
#[derive(Default)]
struct Collection {
    size: u64,
    distance: String,
    indexes: Vec<String>,
    points: BTreeMap<Uuid, Stored>,
}
#[derive(Default)]
struct Mock {
    collections: BTreeMap<String, Collection>,
    /// (method, path) of every request that carried the right API key.
    calls: Vec<(String, String)>,
    /// Answer the next collection lookup as missing, so a create is attempted
    /// against a collection that exists: what a concurrent start sees.
    race: bool,
}
impl Mock {
    fn collection(&self, name: &str) -> &Collection {
        &self.collections[name]
    }
}

fn envelope(result: Value) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], json!({ "result": result, "status": "ok", "time": 0.0 }).to_string())
        .into_response()
}
fn failure(status: StatusCode, error: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        json!({ "status": { "error": error }, "time": 0.0 }).to_string(),
    )
        .into_response()
}
/// The subset of Qdrant's filter language the client uses: `must` with
/// payload `match` and `has_id` conditions.
fn matches(filter: &Value, id: &Uuid, stored: &Stored) -> bool {
    filter["must"].as_array().is_none_or(|must| {
        must.iter().all(|condition| {
            if let Some(ids) = condition.get("has_id") {
                return ids.as_array().unwrap().iter().any(|v| v.as_str() == Some(&id.to_string()));
            }
            let key = condition["key"].as_str().unwrap();
            stored.payload.get(key) == Some(&condition["match"]["value"])
        })
    })
}

async fn mock(State(state): State<Arc<Mutex<Mock>>>, req: Request<Body>) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_owned();
    let wait = req.uri().query() == Some("wait=true");
    if req.headers().get("api-key").and_then(|v| v.to_str().ok()) != Some(API_KEY) {
        return failure(StatusCode::FORBIDDEN, "Must provide an API key or an Authorization bearer token");
    }
    let body = req.into_body().collect().await.unwrap().to_bytes();
    let body: Value = if body.is_empty() { Value::Null } else { serde_json::from_slice(&body).unwrap() };
    let mut mock = state.lock().unwrap();
    mock.calls.push((method.clone(), path.clone()));
    let race = std::mem::take(&mut mock.race);
    let mut segments = path.trim_start_matches("/collections/").splitn(2, '/');
    let name = segments.next().unwrap().to_owned();
    let rest = segments.next().unwrap_or_default();
    match (method.as_str(), rest) {
        ("GET", "") => match mock.collections.get(&name).filter(|_| !race) {
            None => failure(StatusCode::NOT_FOUND, &format!("Not found: Collection `{name}` doesn't exist!")),
            Some(c) => envelope(json!({
                "status": "green",
                "config": { "params": { "vectors": { "size": c.size, "distance": c.distance } } },
                "payload_schema": c.indexes.iter().map(|f| (f.clone(), json!({ "data_type": "keyword", "points": 0 }))).collect::<Map<_, _>>(),
                "points_count": c.points.len(),
            })),
        },
        ("PUT", "") => {
            if mock.collections.contains_key(&name) {
                return failure(StatusCode::CONFLICT, &format!("Collection `{name}` already exists!"));
            }
            let vectors = &body["vectors"];
            let collection = Collection {
                size: vectors["size"].as_u64().unwrap(),
                distance: vectors["distance"].as_str().unwrap().to_owned(),
                ..Default::default()
            };
            mock.collections.insert(name, collection);
            envelope(json!(true))
        }
        ("PUT", "index") => {
            assert_eq!(body["field_schema"], json!({ "type": "keyword", "is_tenant": true }));
            let field = body["field_name"].as_str().unwrap().to_owned();
            mock.collections.get_mut(&name).unwrap().indexes.push(field);
            envelope(json!({ "operation_id": 1, "status": "completed" }))
        }
        ("PUT", "points") => {
            assert!(wait, "writes must wait so a following search sees them");
            let Some(collection) = mock.collections.get_mut(&name) else {
                return failure(StatusCode::NOT_FOUND, "collection missing");
            };
            for point in body["points"].as_array().unwrap() {
                let vector: Vec<f32> = serde_json::from_value(point["vector"].clone()).unwrap();
                if vector.len() as u64 != collection.size {
                    return failure(StatusCode::BAD_REQUEST, "Wrong input: Vector dimension error");
                }
                let id: Uuid = serde_json::from_value(point["id"].clone()).unwrap();
                let payload = point["payload"].as_object().cloned().unwrap_or_default();
                collection.points.insert(id, Stored { vector, payload });
            }
            envelope(json!({ "operation_id": 2, "status": "completed" }))
        }
        ("POST", "points/query") => {
            let collection = mock.collection(&name);
            let query: Vec<f32> = serde_json::from_value(body["query"].clone()).unwrap();
            let limit = body["limit"].as_u64().unwrap() as usize;
            assert_eq!(body["with_payload"], json!(true));
            let mut scored: Vec<(f32, Uuid, &Stored)> = collection
                .points
                .iter()
                .filter(|(id, stored)| matches(&body["filter"], id, stored))
                .map(|(id, stored)| (stored.vector.iter().zip(&query).map(|(a, b)| a * b).sum::<f32>(), *id, stored))
                .collect();
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            let points: Vec<Value> = scored
                .iter()
                .take(limit)
                .map(|(score, id, stored)| json!({ "id": id, "version": 1, "score": score, "payload": stored.payload }))
                .collect();
            envelope(json!({ "points": points }))
        }
        ("POST", "points/delete") => {
            assert!(wait);
            let collection = mock.collections.get_mut(&name).unwrap();
            collection.points.retain(|id, stored| !matches(&body["filter"], id, stored));
            envelope(json!({ "operation_id": 3, "status": "completed" }))
        }
        _ => failure(StatusCode::NOT_FOUND, "unexpected request"),
    }
}

struct Fixture {
    client: Client,
    mock: Arc<Mutex<Mock>>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(cfg: Config) -> Self {
        let state = Arc::new(Mutex::new(Mock::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = Client::new(cfg).unwrap();
        client.base = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new().fallback(any(mock)).with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { client, mock: state, server }
    }
    fn calls(&self) -> Vec<(String, String)> {
        std::mem::take(&mut self.mock.lock().unwrap().calls)
    }
    fn ids(&self) -> Vec<Uuid> {
        self.mock.lock().unwrap().collection("notes").points.keys().copied().collect()
    }
}
fn point(id: Uuid, vector: [f32; 3], title: &str) -> Point {
    let mut payload = Map::new();
    payload.insert("title".into(), json!(title));
    Point { id, vector: vector.to_vec(), payload }
}

#[test]
fn configuration_is_all_or_nothing_and_validated() {
    let parse = |url: &str, key: &str, collection: &str, size: &str, distance: &str| {
        Config::parse(url.into(), key.into(), collection.into(), size.into(), distance.into())
    };
    assert!(parse("", "", "", "", "").unwrap().is_none());
    let cfg = parse("https://abc.cloud.qdrant.io:6333/", "", "", "1536", "").unwrap().unwrap();
    assert_eq!(
        (cfg.url.as_str(), cfg.collection.as_str(), cfg.vector_size),
        ("https://abc.cloud.qdrant.io:6333", "documents", 1536)
    );
    assert_eq!(cfg.distance, Distance::Cosine);
    assert!(cfg.api_key.is_none());
    let cfg = parse("http://localhost:6333", "k", "my-notes_v2", "3", "dot").unwrap().unwrap();
    assert_eq!(
        (cfg.api_key.as_deref(), cfg.collection.as_str(), cfg.distance),
        (Some("k"), "my-notes_v2", Distance::Dot)
    );
    for (url, key, collection, size, distance) in [
        ("", "key", "", "", ""),                   // key without a server
        ("http://localhost:6333", "", "", "", ""), // no vector size
        ("http://localhost:6333", "", "", "0", ""),
        ("http://localhost:6333", "", "", "70000", ""),
        ("http://localhost:6333", "", "", "abc", ""),
        ("localhost:6333", "", "", "3", ""),
        ("ftp://localhost:6333", "", "", "3", ""),
        ("http://localhost:6333/prefix", "", "", "3", ""),
        ("http://localhost:6333?x=1", "", "", "3", ""),
        ("http://localhost:6333", "", "bad name", "3", ""),
        ("http://localhost:6333", "", "", "3", "hamming"),
    ] {
        assert!(parse(url, key, collection, size, distance).is_err(), "{url} {key} {collection} {size} {distance}");
    }
    assert_eq!(Distance::parse("cosine"), Some(Distance::Cosine));
    assert_eq!(Distance::parse("Manhattan").map(Distance::as_str), Some("Manhattan"));
}

#[tokio::test]
async fn startup_creates_the_collection_once_and_refuses_a_mismatched_one() {
    let fixture = Fixture::new(config()).await;
    fixture.client.ensure_collection().await.unwrap();
    assert_eq!(
        fixture.calls(),
        [
            ("GET", "/collections/notes"),
            ("PUT", "/collections/notes"),
            ("GET", "/collections/notes"),
            ("PUT", "/collections/notes/index"),
        ]
        .map(|(m, p)| (m.to_owned(), p.to_owned()))
    );
    {
        let mock = fixture.mock.lock().unwrap();
        let collection = mock.collection("notes");
        assert_eq!((collection.size, collection.distance.as_str()), (3, "Cosine"));
        assert_eq!(collection.indexes, [OWNER_KEY]);
    }
    // A second start finds everything in place and changes nothing.
    fixture.client.ensure_collection().await.unwrap();
    assert_eq!(fixture.calls(), [("GET".to_owned(), "/collections/notes".to_owned())]);
    // Two processes racing to create the collection: the loser's 409 is not an error.
    {
        let mut mock = fixture.mock.lock().unwrap();
        mock.collections.get_mut("notes").unwrap().indexes.clear();
        mock.race = true;
    }
    fixture.client.ensure_collection().await.unwrap();
    assert_eq!(fixture.calls().iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>(), ["GET", "PUT", "GET", "PUT"]);
    assert_eq!(fixture.mock.lock().unwrap().collection("notes").indexes, [OWNER_KEY]);

    // Someone changed the embedding model: the collection no longer fits the configuration.
    let mut mismatched = config();
    mismatched.vector_size = 4;
    let mut other = fixture.client.clone();
    other.cfg = mismatched;
    let error = other.ensure_collection().await.unwrap_err().to_string();
    assert!(error.contains("stores 3 vectors") && error.contains("QDRANT_VECTOR_SIZE"), "{error}");

    // Wrong credentials surface Qdrant's own error text, not a decode failure.
    let mut unauthenticated = fixture.client.clone();
    unauthenticated.cfg.api_key = Some("wrong".into());
    let error = unauthenticated.ensure_collection().await.unwrap_err();
    assert!(format!("{error:#}").contains("HTTP 403 Forbidden: Must provide an API key"), "{error:#}");
}

#[tokio::test]
async fn points_are_only_ever_visible_to_their_owner() {
    let fixture = Fixture::new(config()).await;
    let client = &fixture.client;
    client.ensure_collection().await.unwrap();
    let (alice, bob) = (Uuid::now_v7(), Uuid::now_v7());
    let (a1, a2, b1) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    client
        .upsert(alice, vec![point(a1, [1.0, 0.0, 0.0], "alice one"), point(a2, [0.0, 1.0, 0.0], "alice two")])
        .await
        .unwrap();
    // A caller cannot smuggle another owner in through the payload.
    let mut forged = point(b1, [1.0, 0.0, 0.0], "bob one");
    forged.payload.insert(OWNER_KEY.into(), json!(alice.to_string()));
    client.upsert(bob, vec![forged]).await.unwrap();
    assert_eq!(fixture.mock.lock().unwrap().collection("notes").points[&b1].payload[OWNER_KEY], json!(bob.to_string()));

    // Alice sees only her two points, nearest first, with their payloads.
    let hits = client.search(alice, &[0.9, 0.1, 0.0], 10).await.unwrap();
    assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), [a1, a2]);
    assert!(hits[0].score > hits[1].score);
    assert_eq!(hits[0].payload["title"], "alice one");
    assert_eq!(hits[0].payload[OWNER_KEY], alice.to_string());
    assert_eq!(client.search(alice, &[0.9, 0.1, 0.0], 1).await.unwrap().len(), 1);
    // Bob's identical vector never surfaces for Alice; extra conditions narrow, never widen.
    let hits = client.search(bob, &[1.0, 0.0, 0.0], 10).await.unwrap();
    assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), [b1]);
    let narrowed = client
        .search_filtered(
            alice,
            &[1.0, 0.0, 0.0],
            10,
            vec![json!({ "key": "title", "match": { "value": "alice two" } })],
        )
        .await
        .unwrap();
    assert_eq!(narrowed.iter().map(|h| h.id).collect::<Vec<_>>(), [a2]);
    let stranger = client.search(Uuid::now_v7(), &[1.0, 0.0, 0.0], 10).await.unwrap();
    assert!(stranger.is_empty());

    // Dimension mistakes are caught before any request leaves the process.
    fixture.calls();
    assert!(client.upsert(alice, vec![point(Uuid::now_v7(), [1.0, 0.0, 0.0], "x")]).await.is_ok());
    let short = Point { id: Uuid::now_v7(), vector: vec![1.0, 0.0], payload: Map::new() };
    let error = client.upsert(alice, vec![short]).await.unwrap_err().to_string();
    assert!(error.contains("2 dimensions") && error.contains("stores 3"), "{error}");
    assert!(client.search(alice, &[1.0, 0.0, 0.0, 0.0], 5).await.is_err());
    assert!(client.search(alice, &[f32::NAN, 0.0, 0.0], 5).await.is_err());
    assert!(client.upsert(alice, Vec::new()).await.is_ok());
    assert!(client.delete(alice, &[]).await.is_ok());
    assert_eq!(fixture.calls().len(), 1, "only the valid upsert reached Qdrant");

    // Deleting by id is scoped too: Bob cannot remove Alice's point.
    client.delete(bob, &[a1]).await.unwrap();
    assert!(fixture.ids().contains(&a1));
    client.delete(alice, &[a1, b1]).await.unwrap();
    let remaining = fixture.ids();
    assert!(!remaining.contains(&a1) && remaining.contains(&a2) && remaining.contains(&b1));
    client.delete_all(alice).await.unwrap();
    assert_eq!(fixture.ids(), [b1]);
}

#[sqlx::test(migrations = "../migrations")]
async fn deleting_an_account_purges_its_vectors_through_a_job(pool: sqlx::PgPool) {
    use app_core::jobs::{self, Job as _, Role};
    let fixture = Fixture::new(config()).await;
    fixture.client.ensure_collection().await.unwrap();
    let user = app_auth::users::sign_up(&pool, "owner@example.com", "correct-horse".into()).await.unwrap();
    let keeper = app_auth::users::sign_up(&pool, "keeper@example.com", "correct-horse".into()).await.unwrap();
    let (mine, theirs) = (Uuid::now_v7(), Uuid::now_v7());
    fixture.client.upsert(user.id, vec![point(mine, [1.0, 0.0, 0.0], "mine")]).await.unwrap();
    fixture.client.upsert(keeper.id, vec![point(theirs, [1.0, 0.0, 0.0], "theirs")]).await.unwrap();
    let token = app_auth::sessions::create(&pool, user.id, time::Duration::days(1)).await.unwrap();
    let cfg = app_core::Config {
        site_url: "http://test.local".into(),
        bind_addr: "127.0.0.1:0".into(),
        database_url: String::new(),
        session_ttl_days: 30,
    };
    let (state, app) = crate::build_configured(
        cfg,
        pool.clone(),
        #[cfg(feature = "posthog")]
        None,
        #[cfg(feature = "sentry")]
        None,
        #[cfg(feature = "stripe")]
        None,
        #[cfg(feature = "storage")]
        crate::scratch_storage(),
        Some(fixture.client.clone()),
    );
    let response = app
        .oneshot(
            Request::post("/settings")
                .header(header::ORIGIN, "http://test.local")
                .header(header::COOKIE, format!("session={token}"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("action=delete&confirmation=DELETE&password=correct-horse"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    // The purge is queued with the account's deletion and runs in the worker, so a
    // Qdrant outage never blocks deleting an account.
    let queued =
        sqlx::query!("SELECT kind, dedupe_key FROM jobs WHERE state = 'queued'").fetch_all(&pool).await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, crate::jobs::PurgeVectors::KIND);
    assert_eq!(queued[0].dedupe_key.as_deref(), Some(format!("purge_vectors:{}", user.id).as_str()));
    assert_eq!(fixture.ids().len(), 2, "nothing is deleted inline");

    let (shutdown, rx) = tokio::sync::watch::channel(false);
    let config = jobs::Config {
        role: Role::Both,
        concurrency: 1,
        poll: Duration::from_millis(50),
        shutdown: Duration::from_secs(5),
        lease: Duration::from_secs(60),
    };
    let worker = jobs::Worker::new(pool.clone(), state, crate::jobs::registry(), config);
    let handle = tokio::spawn(worker.run(rx));
    tokio::time::timeout(Duration::from_secs(15), async {
        while fixture.ids().len() != 1 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the worker purges the deleted account's vectors");
    assert_eq!(fixture.ids(), [theirs]);
    let _ = shutdown.send(true);
    let _ = handle.await;
    assert_eq!(
        sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM jobs WHERE state = 'done'"#)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}
