CREATE TABLE sessions (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- SHA-256 of the cookie token; the token itself is never stored.
    token_hash bytea NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    user_agent text,
    ip inet
);

CREATE INDEX sessions_user_id_idx ON sessions (user_id);
CREATE INDEX sessions_expires_at_idx ON sessions (expires_at);
