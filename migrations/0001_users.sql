-- Users sign in with email + password. Emails are stored lower-cased and
-- trimmed by the application; the CHECK makes that invariant enforceable.
CREATE TABLE users (
    id            uuid        PRIMARY KEY,
    email         text        NOT NULL UNIQUE CHECK (email = lower(btrim(email)) AND email <> ''),
    password_hash text        NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);
