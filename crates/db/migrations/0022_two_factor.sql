-- Two-factor login with authenticator apps (TOTP, RFC 6238).

CREATE TABLE user_totp (
    user_id bigint PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    secret bytea NOT NULL,
    -- NULL while setting up: a first code must confirm the app has it.
    enabled_at timestamptz,
    -- The last time step a code was accepted for; later codes only, so
    -- each works once.
    last_step bigint NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- One-time codes for when the app is lost, stored as SHA-256 hashes.
CREATE TABLE user_recovery_codes (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    code_hash bytea NOT NULL,
    used_at timestamptz,
    UNIQUE (user_id, code_hash)
);

-- Logins that passed the password and wait for a code. The browser holds
-- the token in a cookie.
CREATE TABLE login_challenges (
    token_hash bytea PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- Where to go once logged in.
    next text,
    attempts integer NOT NULL DEFAULT 0,
    expires_at timestamptz NOT NULL
);

CREATE INDEX login_challenges_expires_at_idx ON login_challenges (expires_at);
