-- Google sign-in (the `google` Cargo feature). The shared password schema stays
-- compatible: Google-only accounts hold a hash of a discarded 256-bit random
-- password. The tables exist in every build so the feature can be turned on later.
ALTER TABLE users ADD COLUMN google_only boolean NOT NULL DEFAULT false;

CREATE TABLE google_identities (
    user_id uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    subject text NOT NULL UNIQUE CHECK (length(subject) BETWEEN 1 AND 255),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE google_auth_flows (
    state_hash bytea PRIMARY KEY,
    browser_hash bytea NOT NULL,
    nonce text NOT NULL,
    verifier text NOT NULL,
    purpose text NOT NULL CHECK (purpose IN ('login', 'link', 'confirm')),
    user_id uuid REFERENCES users(id) ON DELETE CASCADE,
    session_hash bytea REFERENCES sessions(token_hash) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL DEFAULT now() + interval '10 minutes',
    CHECK ((purpose = 'login' AND user_id IS NULL AND session_hash IS NULL)
        OR (purpose <> 'login' AND user_id IS NOT NULL AND session_hash IS NOT NULL))
);
CREATE INDEX google_auth_flows_expiry_idx ON google_auth_flows(expires_at);
CREATE INDEX google_auth_flows_user_idx ON google_auth_flows(user_id);
CREATE INDEX google_auth_flows_session_idx ON google_auth_flows(session_hash);

-- A confirmation belongs to this exact browser session and dies on sign-out.
CREATE TABLE google_confirmations (
    session_hash bytea PRIMARY KEY REFERENCES sessions(token_hash) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL DEFAULT now() + interval '5 minutes'
);
