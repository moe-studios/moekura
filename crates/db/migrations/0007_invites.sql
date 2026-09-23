-- Invite codes for registration_mode = 'invite'. Only a SHA-256 of each
-- code is stored.
CREATE TABLE invites (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    code_hash bytea NOT NULL UNIQUE CHECK (length(code_hash) = 32),
    -- NULL when created from the command line.
    created_by bigint REFERENCES users (id) ON DELETE SET NULL,
    max_uses integer NOT NULL DEFAULT 1 CHECK (max_uses > 0),
    uses integer NOT NULL DEFAULT 0 CHECK (uses >= 0 AND uses <= max_uses),
    expires_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
