-- Durable background jobs. The SQL lives in core::jobs; this DDL is
-- copied verbatim into every app that uses the crate (the site keeps an
-- identical migration). Payloads hold identifiers, never secrets.
CREATE TABLE jobs (
    id            uuid PRIMARY KEY,
    kind          text NOT NULL,
    payload       jsonb NOT NULL,
    dedupe_key    text,
    state         text NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'running', 'done', 'dead')),
    run_at        timestamptz NOT NULL DEFAULT now(),
    attempts      integer NOT NULL DEFAULT 0,
    max_attempts  integer NOT NULL,
    locked_by     text,
    locked_until  timestamptz,
    last_error    text,
    created_at    timestamptz NOT NULL DEFAULT now(),
    finished_at   timestamptz
);
-- Claim order and the poll query.
CREATE INDEX jobs_ready ON jobs (run_at) WHERE state = 'queued';
-- Lease expiry sweep for crashed workers.
CREATE INDEX jobs_leases ON jobs (locked_until) WHERE state = 'running';
-- Retention sweep.
CREATE INDEX jobs_retention ON jobs (finished_at) WHERE state IN ('done', 'dead');
-- One live job per (kind, dedupe_key); finished jobs free the key again.
CREATE UNIQUE INDEX jobs_dedupe ON jobs (kind, dedupe_key) WHERE dedupe_key IS NOT NULL AND state IN ('queued', 'running');
