-- A token per user for reading feeds without logging in (private sites);
-- only its SHA-256 is stored, like API keys.
ALTER TABLE users ADD COLUMN feed_token_hash bytea UNIQUE;
