-- Optional display name for account settings. Existing accounts start unnamed.
ALTER TABLE users ADD COLUMN name text NOT NULL DEFAULT ''
    CHECK (char_length(name) <= 100);
