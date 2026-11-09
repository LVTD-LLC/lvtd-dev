-- Per-origin ownership and last accepted public URL snapshot. Never contains account URLs.
CREATE TABLE indexnow_state (
    origin text PRIMARY KEY,
    key text NOT NULL,
    snapshot text NOT NULL DEFAULT '{}'
);
