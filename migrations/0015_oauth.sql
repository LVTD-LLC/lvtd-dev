-- OAuth 2.1 authorization server for the JSON API and the MCP server
-- (`app_auth::oauth_server`). Clients register themselves; a user's consent
-- becomes a short-lived code, then a token pair. Only hashes of secrets,
-- codes and tokens are stored, as with sessions and API keys. The tables
-- exist in every build so enabling the `api` feature later needs no migration.
CREATE TABLE oauth_clients (
    id                         text        PRIMARY KEY,
    -- NULL for public clients (token_endpoint_auth_method = none).
    secret_hash                bytea,
    name                       text        NOT NULL,
    client_uri                 text,
    redirect_uris              text[]      NOT NULL,
    token_endpoint_auth_method text        NOT NULL,
    grant_types                text[]      NOT NULL,
    scope                      text,
    created_at                 timestamptz NOT NULL DEFAULT now(),
    -- Touched when a code is issued; clients unused for 90 days are purged.
    last_used_at               timestamptz
);

CREATE TABLE oauth_codes (
    id             uuid        PRIMARY KEY,
    code_hash      bytea       NOT NULL UNIQUE,
    client_id      text        NOT NULL REFERENCES oauth_clients(id) ON DELETE CASCADE,
    user_id        uuid        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    redirect_uri   text        NOT NULL,
    scope          text        NOT NULL,
    resource       text,
    code_challenge text        NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    expires_at     timestamptz NOT NULL
);

CREATE INDEX oauth_codes_expires_at_idx ON oauth_codes (expires_at);

CREATE TABLE oauth_tokens (
    id                 uuid        PRIMARY KEY,
    client_id          text        NOT NULL REFERENCES oauth_clients(id) ON DELETE CASCADE,
    user_id            uuid        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    access_hash        bytea       NOT NULL UNIQUE,
    refresh_hash       bytea       UNIQUE,
    scope              text        NOT NULL,
    resource           text,
    access_expires_at  timestamptz NOT NULL,
    refresh_expires_at timestamptz,
    created_at         timestamptz NOT NULL DEFAULT now(),
    last_used_at       timestamptz
);

CREATE INDEX oauth_tokens_user_id_idx ON oauth_tokens (user_id);
CREATE INDEX oauth_tokens_client_id_idx ON oauth_tokens (client_id);
