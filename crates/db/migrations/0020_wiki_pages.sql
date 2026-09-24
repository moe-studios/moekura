-- Wiki pages, one per tag name. By name rather than tag id, like
-- tag_relations: a page may describe a tag nobody has used yet.
CREATE TABLE wiki_pages (
    id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- Normalised like tag names (moekura_core::tags). The "C" collation
    -- lets the unique index serve prefix searches.
    title text COLLATE "C" NOT NULL UNIQUE CHECK (length(title) BETWEEN 1 AND 170),
    -- moekura_core::markup; MAX_LEN is the real limit.
    body text NOT NULL CHECK (length(body) <= 50000),
    -- The latest entry in wiki_page_versions.
    version integer NOT NULL DEFAULT 1,
    updater_id bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX wiki_pages_updated_at_idx ON wiki_pages (updated_at DESC, id DESC);

-- Every saved text of every page, for history and reverting.
CREATE TABLE wiki_page_versions (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    wiki_page_id integer NOT NULL REFERENCES wiki_pages (id) ON DELETE CASCADE,
    version integer NOT NULL,
    updater_id bigint REFERENCES users (id) ON DELETE SET NULL,
    body text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (wiki_page_id, version)
);

CREATE INDEX wiki_page_versions_updater_id_idx ON wiki_page_versions (updater_id, id DESC)
    WHERE updater_id IS NOT NULL;
