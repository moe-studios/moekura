-- Posts an import from another booru couldn't bring in, and why, so they
-- can be looked into or tried again. Gone once the post is imported.
CREATE TABLE remote_import_failures (
    site text NOT NULL,
    remote_id bigint NOT NULL,
    -- The search that last tried it.
    query text NOT NULL,
    reason text NOT NULL,
    failed_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (site, remote_id)
);
