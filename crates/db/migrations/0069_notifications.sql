-- What someone should know about: being mentioned (@name) or replied to,
-- a new message, a new post in a forum topic they posted in, or a
-- decision on a request they made or voted on.
CREATE TABLE notifications (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('mention', 'reply', 'message', 'forum', 'request')),
    actor_id bigint REFERENCES users (id) ON DELETE SET NULL,
    -- What it's about, for people ("post #12", a topic's title).
    subject text NOT NULL CHECK (length(subject) BETWEEN 1 AND 300),
    -- Where it is: a path on the site.
    url text NOT NULL CHECK (url LIKE '/%'),
    is_read boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX notifications_user_idx ON notifications (user_id, id DESC);
CREATE INDEX notifications_unread_idx ON notifications (user_id) WHERE NOT is_read;
