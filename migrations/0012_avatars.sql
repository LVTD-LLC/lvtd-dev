-- Profile pictures live in object storage or on disk (app_core::storage); users keep only the key.
ALTER TABLE users ADD COLUMN avatar_key text
    CHECK (avatar_key IS NULL OR char_length(avatar_key) BETWEEN 1 AND 512);
