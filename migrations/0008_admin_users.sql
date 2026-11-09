-- Existing accounts and new signups are non-admin until an operator grants access.
ALTER TABLE users ADD COLUMN is_admin boolean NOT NULL DEFAULT false;
