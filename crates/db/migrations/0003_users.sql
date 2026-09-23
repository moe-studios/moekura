CREATE TABLE users (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- Full rules live in uwu_core::accounts::UserName; this is a backstop.
    name citext NOT NULL CHECK (name ~ '^[A-Za-z0-9_.-]{2,32}$'),
    email citext,
    -- NULL for accounts that cannot log in with a password.
    password_hash text,
    role_id integer NOT NULL REFERENCES roles (id),
    -- Status values are a check constraint rather than a Postgres enum:
    -- they are easier to change in later migrations.
    status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'pending', 'deactivated')),
    settings jsonb NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz,
    CONSTRAINT users_name_key UNIQUE (name),
    CONSTRAINT users_email_key UNIQUE (email)
);

CREATE INDEX users_role_id_idx ON users (role_id);
