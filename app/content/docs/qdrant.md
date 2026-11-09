+++
title = "Vector search with Qdrant"
slug = "qdrant"
description = "Store embeddings in Qdrant and search them per user, with your own embedding model."
section = "features"
order = 55
+++

## What is included

The optional `qdrant` Cargo feature connects the app to a [Qdrant](https://qdrant.tech) vector database. It adds `app/src/vectors.rs`: runtime configuration, a small client over Qdrant's REST API, and the rules that keep each user's vectors private. Enable it in `app/Cargo.toml` or run `cargo run --features qdrant`.

- One collection for the whole app, created at startup when it does not exist, with a payload index on the owning user.
- `upsert`, `search`, `delete` and `delete_all`, every one scoped to a user id. A user can only search, read or delete their own points.
- Account deletion queues a `purge_vectors` background job that removes everything the user stored.
- No new crates: the client uses the `reqwest` and `serde_json` dependencies the app already has.

The module stores and searches vectors; it does not create them. Pick an embedding model (an API such as OpenAI, Voyage or Cohere, or a local model), call it from your own code, and hand the resulting `Vec<f32>` to the client. Every vector in the collection must come from the same model, because its dimension and distance are fixed when the collection is created.

Leaving every `QDRANT_*` setting blank keeps the app runnable with `state.vectors` set to `None`. Partial configuration fails startup, as does a collection whose vectors do not match the configured size or distance.

## Run Qdrant

Locally, `docker-compose.yml` includes a Qdrant service behind the `qdrant` profile so it only starts when you ask for it:

```sh
docker compose --profile qdrant up -d   # Qdrant on localhost:6333, Postgres as usual
```

For production, use [Qdrant Cloud](https://cloud.qdrant.io) or run the `qdrant/qdrant` image next to the app. Qdrant 1.11 or newer is required for the tenant index the app creates.

## Configure

Set these values in your ignored `.env` file or your host's environment:

```dotenv
QDRANT_URL=http://localhost:6333
QDRANT_API_KEY=
QDRANT_COLLECTION=documents
QDRANT_VECTOR_SIZE=1536
QDRANT_DISTANCE=Cosine
```

| Variable | Meaning |
| --- | --- |
| `QDRANT_URL` | The server's origin, without a path. Qdrant Cloud clusters look like `https://xyz.cloud.qdrant.io:6333`. Blank disables the integration. |
| `QDRANT_API_KEY` | Required by Qdrant Cloud and by self-hosted servers started with an API key. Blank for a local server without one. Sent as the `api-key` header; keep it on the server. |
| `QDRANT_COLLECTION` | Collection name. Defaults to `documents`. |
| `QDRANT_VECTOR_SIZE` | Dimension of your embedding model's output, for example 1536 for `text-embedding-3-small` or 1024 for `voyage-3`. Required. |
| `QDRANT_DISTANCE` | `Cosine` (default), `Euclid`, `Dot` or `Manhattan`. Use what your embedding model recommends. |

At startup the app reaches Qdrant, creates the collection if it is missing, checks that an existing one stores vectors of the configured size and distance, and creates a keyword index on the owner field. Changing the embedding model later means a new collection name (or deleting the old collection) and re-embedding your data.

## Use it in your product

`state.vectors` is `Some(client)` when Qdrant is configured. Scope every call to the signed-in user: the client writes the owner into each point's payload and filters every query by it, so this is the only id you pass.

```rust,ignore
use crate::vectors::{Hit, Point};
use serde_json::json;

async fn embed(text: &str) -> anyhow::Result<Vec<f32>> {
    // Call your embedding model here. The vector must have QDRANT_VECTOR_SIZE dimensions.
    todo!("embed {text}")
}

/// Store a note's embedding. Keep the note itself in Postgres; the payload only
/// carries what a search result needs to show.
async fn index_note(state: &SharedState, user: &User, note_id: Uuid, title: &str, body: &str) -> anyhow::Result<()> {
    let Some(vectors) = &state.vectors else { return Ok(()) };
    let mut payload = serde_json::Map::new();
    payload.insert("note_id".into(), json!(note_id));
    payload.insert("title".into(), json!(title));
    vectors.upsert(user.id, vec![Point { id: note_id, vector: embed(body).await?, payload }]).await
}

/// The user's ten notes closest to a query.
async fn search_notes(state: &SharedState, user: &User, query: &str) -> anyhow::Result<Vec<Hit>> {
    let Some(vectors) = &state.vectors else { return Ok(Vec::new()) };
    vectors.search(user.id, &embed(query).await?, 10).await
}
```

Each `Hit` has the point `id`, a `score` (higher is closer for cosine and dot distance) and the stored `payload`. Load the full records from Postgres by the ids in the payload before rendering.

Reusing the Postgres row id as the point id, as above, makes updates and deletions one call: `vectors.delete(user.id, &[note_id])` when the note is deleted, `upsert` again when it changes. `search_filtered` adds Qdrant `must` conditions on the payload (for example `{"key": "kind", "match": {"value": "note"}}`) without ever widening the owner scope; `Client::owner_condition` gives you that condition for custom requests of your own.

Indexing from a request handler is fine for one document. For bulk imports or slow embedding models, enqueue a [background job](/docs/background-jobs) that embeds and upserts, so the user is not waiting on the model.

## Privacy and deletion

Payloads are returned with every search, so keep private content in Postgres and store only what the result needs (titles, foreign keys). The owner id is a UUID, never an email.

Deleting an account enqueues `jobs::PurgeVectors` inside the same transaction, and the worker calls `delete_all` for that user. A Qdrant outage delays the purge rather than blocking the deletion; the job retries with backoff. The worker process needs the same `QDRANT_*` settings as the web process; a worker without them completes the job without deleting anything.

## Tests

The test suite runs the client against an in-process double of Qdrant's REST endpoints, so it needs no running server and never touches a real collection:

```sh
cargo test --workspace --features qdrant
cargo test --workspace --no-default-features --features qdrant
```

To exercise a real server, start the compose profile above and try your own handlers; there is no live test in the suite by design.
