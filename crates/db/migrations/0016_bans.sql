-- Banned users keep their account and can look around as visitors do,
-- but can't act. A ban is active until it expires or is lifted.
CREATE TABLE bans (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 2000),
    banner_id bigint REFERENCES users (id) ON DELETE SET NULL,
    -- NULL: until lifted.
    expires_at timestamptz,
    lifted_at timestamptz,
    lifter_id bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX bans_user_idx ON bans (user_id, id DESC);

-- Networks that may not register, log in or post.
CREATE TABLE ip_bans (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    network cidr NOT NULL,
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 2000),
    banner_id bigint REFERENCES users (id) ON DELETE SET NULL,
    expires_at timestamptz,
    lifted_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- "Which ban covers this address?" (network >>= address).
CREATE INDEX ip_bans_network_idx ON ip_bans USING gist (network inet_ops) WHERE lifted_at IS NULL;
