-- Reports that a post breaks the rules. An open flag marks the post
-- flagged (still visible) until a moderator dismisses the flags or
-- deletes the post.
CREATE TABLE post_flags (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    post_id bigint NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    creator_id bigint REFERENCES users (id) ON DELETE SET NULL,
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 2000),
    status text NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'dismissed', 'upheld')),
    resolver_id bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    resolved_at timestamptz
);

-- One open flag per person and post.
CREATE UNIQUE INDEX post_flags_open_idx ON post_flags (post_id, creator_id) WHERE status = 'open';
CREATE INDEX post_flags_post_idx ON post_flags (post_id, id);
