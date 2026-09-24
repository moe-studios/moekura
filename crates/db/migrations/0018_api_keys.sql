-- API keys: long-lived credentials for scripts, acting as their user.
CREATE TABLE api_keys (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 64),
    -- SHA-256 of the key; the key itself is shown once and never stored.
    token_hash bytea NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    -- The key's first characters, so its owner can tell keys apart.
    prefix text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    expires_at timestamptz
);

CREATE INDEX api_keys_user_id_idx ON api_keys (user_id, id);
