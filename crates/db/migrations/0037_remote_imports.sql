-- Imports from other boorus (moekura admin import-remote): where each
-- site and search got to, so a stopped import carries on...
CREATE TABLE remote_imports (
    id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- The site's base URL.
    site text NOT NULL,
    query text NOT NULL,
    -- moekura_core::remote::Cursor as text: start, b<id> or p<page>.
    cursor text NOT NULL DEFAULT 'start',
    imported integer NOT NULL DEFAULT 0,
    duplicates integer NOT NULL DEFAULT 0,
    failed integer NOT NULL DEFAULT 0,
    finished boolean NOT NULL DEFAULT false,
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (site, query)
);

-- ...and which local post each imported post became, to link parents,
-- notes and pools.
CREATE TABLE remote_posts (
    site text NOT NULL,
    remote_id bigint NOT NULL,
    post_id bigint NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    remote_parent_id bigint,
    PRIMARY KEY (site, remote_id)
);

CREATE INDEX remote_posts_parent_idx ON remote_posts (site, remote_parent_id)
    WHERE remote_parent_id IS NOT NULL;
