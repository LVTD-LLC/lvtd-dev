-- Personal API keys for the optional JSON API (`api` feature). The client
-- holds a random token; only its SHA-256 is stored, as with sessions. The
-- table exists in every build so enabling the feature later needs no migration.
CREATE TABLE api_keys (
    id           uuid        PRIMARY KEY,
    user_id      uuid        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash   bytea       NOT NULL UNIQUE,
    -- The first characters of the token, so a key can be recognized in the UI.
    prefix       text        NOT NULL,
    name         text        NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz
);

CREATE INDEX api_keys_user_id_idx ON api_keys (user_id);
