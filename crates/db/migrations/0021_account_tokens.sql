-- Email verification and password resets.

-- 'unverified': registered on a site that checks addresses, and hasn't
-- followed the link sent to theirs yet; can't log in.
ALTER TABLE users DROP CONSTRAINT users_status_check;
ALTER TABLE users ADD CONSTRAINT users_status_check
    CHECK (status IN ('active', 'pending', 'unverified', 'deactivated'));

-- When the current address was confirmed by following a link sent to it.
ALTER TABLE users ADD COLUMN email_verified_at timestamptz;

-- Single-use links sent by email. Only a SHA-256 of the token is kept
-- (moekura_core::tokens).
CREATE TABLE account_tokens (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    purpose text NOT NULL CHECK (purpose IN ('verify_email', 'reset_password')),
    token_hash bytea NOT NULL UNIQUE,
    -- The address the link went to: for verify_email, the one that
    -- becomes the account's address.
    email citext NOT NULL,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX account_tokens_user_id_idx ON account_tokens (user_id, purpose);
CREATE INDEX account_tokens_expires_at_idx ON account_tokens (expires_at);
