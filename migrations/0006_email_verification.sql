-- Pending signups are deliberately not users: no account/session exists before proof.
CREATE TABLE pending_signups (
    email text PRIMARY KEY,
    password_hash text NOT NULL,
    token_hash bytea NOT NULL UNIQUE,
    code_hash bytea NOT NULL,
    expires_at timestamptz NOT NULL,
    last_sent_at timestamptz NOT NULL DEFAULT now(),
    attempts integer NOT NULL DEFAULT 0,
    delivered boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX pending_signups_expiry ON pending_signups (expires_at);
CREATE TABLE signup_limits (
    key bytea PRIMARY KEY,
    window_start timestamptz NOT NULL DEFAULT now(),
    requests integer NOT NULL DEFAULT 1
);
