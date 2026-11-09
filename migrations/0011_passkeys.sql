-- Passkeys (WebAuthn discoverable credentials). Only public keys are stored;
-- the private key never leaves the user's authenticator.
CREATE TABLE passkeys (
    id            uuid        PRIMARY KEY,
    user_id       uuid        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    credential_id bytea       NOT NULL UNIQUE,
    public_key    bytea       NOT NULL,
    algorithm     integer     NOT NULL,
    sign_count    bigint      NOT NULL DEFAULT 0,
    transports    text[]      NOT NULL DEFAULT '{}',
    backed_up     boolean     NOT NULL DEFAULT false,
    name          text        NOT NULL CHECK (char_length(name) BETWEEN 1 AND 64),
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_used_at  timestamptz
);
CREATE INDEX passkeys_user_id_idx ON passkeys (user_id);

-- Single-use ceremony challenges. A registration challenge is bound to the
-- signed-in user; a sign-in challenge is anonymous. Expired rows are swept
-- whenever a new challenge is issued.
CREATE TABLE passkey_challenges (
    challenge  bytea       PRIMARY KEY,
    user_id    uuid        REFERENCES users(id) ON DELETE CASCADE,
    purpose    text        NOT NULL CHECK (purpose IN ('register', 'sign_in')),
    expires_at timestamptz NOT NULL
);
CREATE INDEX passkey_challenges_expires_at_idx ON passkey_challenges (expires_at);
