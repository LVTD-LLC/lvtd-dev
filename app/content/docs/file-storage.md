+++
title = "File storage"
slug = "file-storage"
description = "Store user uploads in Cloudflare R2, AWS S3 or any S3-compatible bucket, or on local disk, with profile pictures as the example."
section = "features"
order = 80
+++

## What it does

The optional `storage` Cargo feature (on by default) gives the app one place to put files users upload: `state.storage`, an `app_core::storage::Storage`. With no configuration it writes to a directory on the machine running the app. Add a bucket's settings and the same code writes to Cloudflare R2, AWS S3, MinIO, Backblaze B2 or any other S3-compatible service instead. Nothing else changes.

Profile pictures in **Account settings** are the built-in example: PNG, JPEG, GIF or WebP up to 2 MB, shown next to the account menu. `app/src/avatars.rs` is the pattern to copy for other uploads.

## Local disk (the default)

Leave every `STORAGE_S3_*` value in `.env` blank. Uploads land under `STORAGE_LOCAL_DIR`, which defaults to `./storage` next to the project and to `/app/storage` inside the Docker image. The app serves them itself at `/avatars/…`.

A container's disk does not survive a redeploy: in production, either mount a persistent volume at `/app/storage` or use a bucket.

## Cloudflare R2

1. In the Cloudflare dashboard, open **R2** and create a bucket.
2. Under **Manage R2 API Tokens**, create a token with **Object Read & Write** on that bucket. Copy the access key ID and secret access key.
3. Set these values in `.env` or your host's environment:

```dotenv
STORAGE_S3_BUCKET=my-uploads
STORAGE_S3_ENDPOINT=https://<account-id>.r2.cloudflarestorage.com
STORAGE_S3_REGION=auto
STORAGE_S3_ACCESS_KEY_ID=...
STORAGE_S3_SECRET_ACCESS_KEY=...
```

The account id is on the R2 overview page. To have browsers load files straight from R2 instead of through the app, enable public access on the bucket (a custom domain or the `r2.dev` subdomain) and set `STORAGE_S3_PUBLIC_URL` to that origin, for example `https://files.example.com`.

## AWS S3 and other services

The same four settings work everywhere. For AWS use the bucket's regional endpoint and region:

```dotenv
STORAGE_S3_ENDPOINT=https://s3.eu-west-1.amazonaws.com
STORAGE_S3_REGION=eu-west-1
```

MinIO, Backblaze B2 and similar services publish an endpoint and keys the same way; keep `STORAGE_S3_REGION=auto` unless the service requires its own. Requests use path-style addressing (`<endpoint>/<bucket>/<key>`) and Signature Version 4, which all of them accept. Setting some but not all of the four values stops the app at startup with a message saying which are missing.

## Storing your own files

```rust
use app_core::storage::Storage;

// Write, read and delete by key. Keys are `/`-separated segments of
// letters, digits, `.`, `_` and `-`; anything else is rejected before it
// reaches the disk or the bucket.
state.storage.put("invoices/2026/0042.pdf", "application/pdf", bytes).await?;
let object = state.storage.get("invoices/2026/0042.pdf").await?; // None if missing
state.storage.delete("invoices/2026/0042.pdf").await?;

// Where a browser can load it from: the bucket's public URL when there is
// one, otherwise a route of your own that calls `get`.
let link = state.storage.public_url(key);
```

Follow `app/src/avatars.rs`: use a key prefix per kind of file, validate the bytes before `put` (the content type a browser sends is a hint, not a fact), keep the key on the row that owns the file, delete the previous object once the row points elsewhere, and add a prefix-scoped route like `GET /avatars/{*path}` for files the app should serve. Objects are read and written whole, so this is for uploads of a few megabytes; for large files, hand the browser a presigned URL instead.
