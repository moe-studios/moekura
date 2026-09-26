-- Mass tag edits: tags added to and taken off every post matching a
-- search, by a job, with its progress.
CREATE TABLE mass_updates (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    creator_id bigint REFERENCES users (id) ON DELETE SET NULL,
    query text NOT NULL CHECK (length(query) BETWEEN 1 AND 1000),
    add_tags text[] NOT NULL DEFAULT '{}',
    remove_tags text[] NOT NULL DEFAULT '{}',
    status text NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'done', 'failed')),
    -- Posts looked at and changed so far.
    seen integer NOT NULL DEFAULT 0,
    changed integer NOT NULL DEFAULT 0,
    error text,
    created_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz
);

CREATE INDEX mass_updates_recent_idx ON mass_updates (id DESC);

-- The new mass_edit_tags permission (bit 21) for moderators, as
-- SystemRole::default_permissions has it.
UPDATE roles SET permissions = permissions | (1::bigint << 21) WHERE system_key = 'moderator';
