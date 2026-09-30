-- Artist entries (moekura_core::artists): the other names, group and
-- URLs of the artist whose tag is `name`, with their history.
CREATE TABLE artists (
    id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- The artist tag's name, normalised like tag names.
    name text COLLATE "C" NOT NULL CHECK (length(name) BETWEEN 1 AND 170),
    group_name text NOT NULL DEFAULT '' CHECK (length(group_name) <= 170),
    other_names text[] NOT NULL DEFAULT '{}' CHECK (cardinality(other_names) <= 50),
    -- Banned artists' posts are hidden or refused (site setting
    -- banned_artists).
    is_banned boolean NOT NULL DEFAULT false,
    is_deleted boolean NOT NULL DEFAULT false,
    -- The latest entry in artist_versions.
    version integer NOT NULL DEFAULT 1,
    updater_id bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX artists_name_idx ON artists (name);
CREATE INDEX artists_updated_at_idx ON artists (updated_at DESC, id DESC);
CREATE INDEX artists_other_names_idx ON artists USING gin (other_names);
CREATE INDEX artists_group_name_idx ON artists (lower(group_name)) WHERE group_name <> '';
CREATE INDEX artists_banned_idx ON artists (id) WHERE is_banned AND NOT is_deleted;

CREATE TABLE artist_urls (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    artist_id integer NOT NULL REFERENCES artists (id) ON DELETE CASCADE,
    -- In order, as entered.
    position integer NOT NULL,
    url text NOT NULL CHECK (length(url) <= 2048),
    -- moekura_core::artists::normalize_url, for finding the artist of a URL.
    normalized_url text NOT NULL,
    is_active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (artist_id, position)
);

CREATE INDEX artist_urls_normalized_idx ON artist_urls (normalized_url);
CREATE INDEX artist_urls_artist_idx ON artist_urls (artist_id);

CREATE TABLE artist_versions (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    artist_id integer NOT NULL REFERENCES artists (id) ON DELETE CASCADE,
    version integer NOT NULL,
    updater_id bigint REFERENCES users (id) ON DELETE SET NULL,
    name text NOT NULL,
    group_name text NOT NULL,
    other_names text[] NOT NULL,
    -- Inactive ones start with `-`, as on Danbooru.
    urls text[] NOT NULL,
    is_banned boolean NOT NULL,
    is_deleted boolean NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (artist_id, version)
);

CREATE INDEX artist_versions_updater_idx ON artist_versions (updater_id, id DESC)
    WHERE updater_id IS NOT NULL;
