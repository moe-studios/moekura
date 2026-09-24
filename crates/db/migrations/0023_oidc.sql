-- Logging in through an OpenID Connect provider.

-- Which provider accounts log in as which users.
CREATE TABLE user_identities (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    issuer text NOT NULL,
    -- The provider's stable id for the account (the `sub` claim).
    subject text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (issuer, subject)
);

CREATE INDEX user_identities_user_id_idx ON user_identities (user_id);

-- Logins sent to the provider and not back yet. `state` comes back in the
-- redirect; only its hash is kept.
CREATE TABLE oidc_logins (
    state_hash bytea PRIMARY KEY,
    nonce text NOT NULL,
    -- PKCE: proves to the provider that whoever redeems the code started
    -- the login.
    code_verifier text NOT NULL,
    next text,
    -- Set when a logged-in user is linking the provider account to theirs.
    link_user_id bigint REFERENCES users (id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL
);

CREATE INDEX oidc_logins_expires_at_idx ON oidc_logins (expires_at);
