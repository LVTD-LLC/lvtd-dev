//! Profile pictures: the worked example of file storage
//! (`app_core::storage`, the `storage` Cargo feature). Images are checked by their bytes, stored
//! under `avatars/<user id>/<random>.<ext>`, linked from the bucket's public
//! URL when it has one and otherwise served by [`serve`] at `/avatars/…`.
//! Other uploads follow the same shape: a key prefix per kind, validation
//! before `put`, the key on the owning row, the old object deleted after.

use crate::{AppError, SharedState, settings};
use app_auth::CurrentUser;
use app_core::storage::Storage;
use axum::body::Bytes;
use axum::extract::{Multipart, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use sqlx::PgPool;
use uuid::Uuid;

/// Keys of profile pictures; the only prefix [`serve`] hands out.
pub const PREFIX: &str = "avatars/";
/// The largest image accepted.
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
/// Body limit for the upload route: the image plus multipart framing.
pub const BODY_LIMIT: usize = MAX_BYTES + 64 * 1024;
/// The multipart field carrying the image.
pub const FIELD: &str = "image";

const ACCEPTED: &str = "Choose a PNG, JPEG, GIF or WebP image.";
const TOO_LARGE: &str = "That image is too large. Use one under 2 MB.";

/// The image type from its first bytes, never from the client's content type
/// or file name: PNG, JPEG, GIF or WebP, as (media type, extension).
pub fn image_type(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", "png"))
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(("image/jpeg", "jpg"))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(("image/gif", "gif"))
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(("image/webp", "webp"))
    } else {
        None
    }
}

/// Where the browser loads a stored picture from.
pub fn url(storage: &Storage, key: &str) -> String {
    storage.public_url(key).unwrap_or_else(|| format!("/{key}"))
}

/// Point the account at `key` (or at nothing) and return the previous key.
async fn set_key(pool: &PgPool, user_id: Uuid, key: Option<&str>) -> Result<Option<String>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let previous = sqlx::query_scalar!("SELECT avatar_key FROM users WHERE id = $1 FOR UPDATE", user_id)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query!("UPDATE users SET avatar_key = $2, updated_at = now() WHERE id = $1", user_id, key)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(previous)
}

/// Delete an object nothing points at any more. Best effort: a leftover
/// costs storage, not correctness.
pub async fn discard(storage: &Storage, key: Option<String>) {
    if let Some(key) = key
        && let Err(error) = storage.delete(&key).await
    {
        tracing::warn!(error = %error, "could not delete a replaced profile picture");
    }
}

fn problem(error: &axum::extract::multipart::MultipartError) -> &'static str {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE { TOO_LARGE } else { ACCEPTED }
}

/// The `image` field's bytes, or what is wrong with the request.
async fn image_field(multipart: &mut Multipart) -> Result<Bytes, &'static str> {
    loop {
        let Some(field) = multipart.next_field().await.map_err(|error| problem(&error))? else {
            return Err(ACCEPTED);
        };
        if field.name() != Some(FIELD) {
            continue;
        }
        let bytes = field.bytes().await.map_err(|error| problem(&error))?;
        return if bytes.len() > MAX_BYTES {
            Err(TOO_LARGE)
        } else if bytes.is_empty() {
            Err(ACCEPTED)
        } else {
            Ok(bytes)
        };
    }
}

/// `POST /settings/avatar`, multipart with an `image` field. Re-renders the
/// settings page with the problem (422) or redirects back to it.
pub async fn upload(
    State(state): State<SharedState>,
    CurrentUser(user): CurrentUser,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let image = image_field(&mut multipart)
        .await
        .and_then(|bytes| image_type(&bytes).map(|(content_type, ext)| (bytes, content_type, ext)).ok_or(ACCEPTED));
    let (bytes, content_type, extension) = match image {
        Ok(image) => image,
        Err(message) => return settings::problem(&state, &user, StatusCode::UNPROCESSABLE_ENTITY, message).await,
    };
    // A random part per upload: the URL changes, so browsers never show a stale picture.
    let mut random = [0u8; 8];
    getrandom::fill(&mut random)?;
    let key =
        format!("{PREFIX}{}/{}.{extension}", user.id, random.iter().map(|b| format!("{b:02x}")).collect::<String>());
    state.storage.put(&key, content_type, bytes).await?;
    let previous = set_key(&state.pool, user.id, Some(&key)).await?;
    discard(&state.storage, previous.filter(|previous| previous != &key)).await;
    Ok(Redirect::to("/settings?saved=avatar").into_response())
}

/// The `remove_avatar` action of the settings form.
pub async fn remove(state: &SharedState, user_id: Uuid) -> anyhow::Result<()> {
    let previous = set_key(&state.pool, user_id, None).await?;
    discard(&state.storage, previous).await;
    Ok(())
}

/// `GET /avatars/{*path}`: pictures through the app when the bucket has no
/// public URL, and always on local disk. Keys carry a random part, so the
/// response is cacheable for good. Malformed keys and other prefixes are 404s.
pub async fn serve(State(state): State<SharedState>, Path(path): Path<String>) -> Result<Response, AppError> {
    match state.storage.get(&format!("{PREFIX}{path}")).await? {
        Some(object) => Ok((
            [
                (header::CONTENT_TYPE, object.content_type),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_owned()),
            ],
            object.bytes,
        )
            .into_response()),
        None => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_recognized_by_their_bytes_only() {
        assert_eq!(image_type(b"\x89PNG\r\n\x1a\nrest"), Some(("image/png", "png")));
        assert_eq!(image_type(b"\xff\xd8\xff\xe0JFIF"), Some(("image/jpeg", "jpg")));
        assert_eq!(image_type(b"GIF89a..."), Some(("image/gif", "gif")));
        assert_eq!(image_type(b"RIFF\x10\x00\x00\x00WEBPVP8 "), Some(("image/webp", "webp")));
        for bytes in
            [&b""[..], b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>", b"<!doctype html>", b"RIFF....WAVE", b"\x89PN"]
        {
            assert_eq!(image_type(bytes), None, "{bytes:?}");
        }
    }
}
