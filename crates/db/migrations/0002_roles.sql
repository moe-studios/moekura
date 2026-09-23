-- Roles and their permission sets. See uwu_core::permissions for the bit
-- layout of `permissions`; bits are never reused.
CREATE TABLE roles (
    id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- Display name; admins may rename any role.
    name citext NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 32),
    permissions bigint NOT NULL DEFAULT 0,
    -- Higher ranks may act on users with lower ranks.
    rank smallint NOT NULL,
    -- Identifies built-in roles to code (uwu_core::permissions::SystemRole).
    system_key text UNIQUE CHECK (
        system_key IN ('anonymous', 'member', 'contributor', 'janitor', 'moderator', 'admin')
    ),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Must match SystemRole::default_permissions (checked by a test).
INSERT INTO roles (name, permissions, rank, system_key) VALUES
    ('Anonymous',   1,      0,  'anonymous'),
    ('Member',      255,    10, 'member'),
    ('Contributor', 131327, 20, 'contributor'),
    ('Janitor',     138239, 30, 'janitor'),
    ('Moderator',   211967, 40, 'moderator'),
    -- All bits set, so admins also get permissions added later.
    ('Admin',       -1,     50, 'admin');
