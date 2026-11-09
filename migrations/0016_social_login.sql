-- Provider-neutral sign-in identities (the `google` and `github` Cargo
-- features), replacing 0014's Google-only tables. A provider is code (a
-- `social_login::Client`) and configuration, never new schema. The tables exist
-- in every build so providers can be turned on later. Pending flows and
-- confirmations are short-lived and are dropped rather than copied.
ALTER TABLE users RENAME COLUMN google_only TO passwordless;

CREATE TABLE social_identities (
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider text NOT NULL CHECK (provider ~ '^[a-z][a-z0-9-]{0,31}$'),
    subject text NOT NULL CHECK (length(subject) BETWEEN 1 AND 255),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, provider),
    UNIQUE (provider, subject)
);
INSERT INTO social_identities (user_id, provider, subject, created_at)
    SELECT user_id, 'google', subject, created_at FROM google_identities;

CREATE TABLE social_login_flows (
    state_hash bytea PRIMARY KEY,
    provider text NOT NULL CHECK (provider ~ '^[a-z][a-z0-9-]{0,31}$'),
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
CREATE INDEX social_login_flows_expiry_idx ON social_login_flows(expires_at);
CREATE INDEX social_login_flows_user_idx ON social_login_flows(user_id);
CREATE INDEX social_login_flows_session_idx ON social_login_flows(session_hash);

-- A confirmation (through any linked provider) belongs to this exact browser
-- session and dies on sign-out.
CREATE TABLE social_login_confirmations (
    session_hash bytea PRIMARY KEY REFERENCES sessions(token_hash) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL DEFAULT now() + interval '5 minutes'
);

DROP TABLE google_confirmations;
DROP TABLE google_auth_flows;
DROP TABLE google_identities;
