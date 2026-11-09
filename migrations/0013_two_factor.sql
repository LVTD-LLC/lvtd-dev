-- Two-factor authentication (TOTP, RFC 6238). One secret per account; the
-- row exists unconfirmed while the user scans the QR code and becomes the
-- second factor once a code from the authenticator app has been verified.
-- The secret is stored as-is: it must be recoverable to check codes.
CREATE TABLE totp_secrets (
    user_id        uuid        PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    secret         bytea       NOT NULL,
    confirmed_at   timestamptz,
    -- The last 30-second step a code was accepted for; a code is never accepted twice.
    last_used_step bigint      NOT NULL DEFAULT 0,
    created_at     timestamptz NOT NULL DEFAULT now()
);

-- Single-use recovery codes for a lost authenticator. Only SHA-256 hashes
-- are stored; the plaintext is shown once, when the codes are generated.
CREATE TABLE recovery_codes (
    id         uuid        PRIMARY KEY,
    user_id    uuid        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash  bytea       NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    used_at    timestamptz
);
CREATE INDEX recovery_codes_user_id_idx ON recovery_codes (user_id);

-- A sign-in that passed the password check and is waiting for the code.
-- The browser holds a random token in a cookie; only its SHA-256 is stored.
-- Expired rows are swept whenever a new challenge is issued.
CREATE TABLE two_factor_challenges (
    token_hash bytea       PRIMARY KEY,
    user_id    uuid        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    attempts   integer     NOT NULL DEFAULT 0,
    expires_at timestamptz NOT NULL
);
CREATE INDEX two_factor_challenges_expires_at_idx ON two_factor_challenges (expires_at);
