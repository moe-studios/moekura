-- Passkeys (WebAuthn): logging in with a device's screen lock or a
-- security key, without a password or instead of a two-factor code.

CREATE TABLE user_passkeys (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- As the authenticator names it; a passkey is registered once, to
    -- one account.
    credential_id bytea NOT NULL UNIQUE,
    -- The random user handle the authenticator keeps with the passkey and
    -- gives back when it picks one at login. The same for all of a user's
    -- passkeys.
    user_handle bytea NOT NULL,
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 64),
    -- The credential as webauthn-rs keeps it: public key and algorithm,
    -- sign count, transports, backup flags.
    credential jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz
);

CREATE INDEX user_passkeys_user_id_idx ON user_passkeys (user_id);

-- Registrations and logins waiting for the browser's answer: what the
-- server asked, so the answer can be checked against it, once. The
-- browser holds the token.
CREATE TABLE passkey_challenges (
    token_hash bytea PRIMARY KEY,
    -- NULL for a login that hasn't said whose passkey it is yet.
    user_id bigint REFERENCES users (id) ON DELETE CASCADE,
    purpose text NOT NULL CHECK (purpose IN ('register', 'login', 'second_factor')),
    state jsonb NOT NULL,
    expires_at timestamptz NOT NULL
);

CREATE INDEX passkey_challenges_expires_at_idx ON passkey_challenges (expires_at);
