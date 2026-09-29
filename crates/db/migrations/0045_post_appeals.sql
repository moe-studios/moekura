-- Requests to bring back a deleted post. Staff approve one by restoring
-- the post, or reject it.
CREATE TABLE post_appeals (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    post_id bigint NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    creator_id bigint REFERENCES users (id) ON DELETE SET NULL,
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 2000),
    status text NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'approved', 'rejected')),
    resolver_id bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    resolved_at timestamptz
);

-- One open appeal per post.
CREATE UNIQUE INDEX post_appeals_open_idx ON post_appeals (post_id) WHERE status = 'open';
CREATE INDEX post_appeals_post_idx ON post_appeals (post_id, id);
CREATE INDEX post_appeals_queue_idx ON post_appeals (id) WHERE status = 'open';
