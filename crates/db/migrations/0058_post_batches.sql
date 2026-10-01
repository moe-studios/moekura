-- Moderation of many posts at once, by a job in batches, with its
-- progress: deleting every upload of a user.
CREATE TABLE post_batches (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('delete')),
    creator_id bigint REFERENCES users (id) ON DELETE SET NULL,
    -- Whose uploads are deleted.
    user_id bigint REFERENCES users (id) ON DELETE SET NULL,
    reason text NOT NULL DEFAULT '' CHECK (length(reason) <= 2000),
    -- Whether the creator could change posts whose status is locked.
    override_locks boolean NOT NULL DEFAULT false,
    status text NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'done', 'failed')),
    -- Posts it applied to when started, then those done, skipped (dealt
    -- with meanwhile, or locked) and failed so far.
    total integer NOT NULL DEFAULT 0,
    done integer NOT NULL DEFAULT 0,
    skipped integer NOT NULL DEFAULT 0,
    failed integer NOT NULL DEFAULT 0,
    -- Posts are handled newest first; those below this id are left, so a
    -- retried job carries on where it stopped.
    resume_before bigint,
    error text,
    created_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz
);

CREATE INDEX post_batches_recent_idx ON post_batches (id DESC);
CREATE INDEX post_batches_user_idx ON post_batches (user_id, id DESC) WHERE user_id IS NOT NULL;
