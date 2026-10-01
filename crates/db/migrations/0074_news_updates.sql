-- Site news: short announcements admins post, shown at the top of every
-- page until each reader dismisses them or they expire.
CREATE TABLE news_updates (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- moekura_core::markup.
    body text NOT NULL CHECK (length(body) BETWEEN 1 AND 2000),
    creator_id bigint REFERENCES users (id) ON DELETE SET NULL,
    -- NULL until deleted or replaced by newer news.
    expires_at timestamptz,
    is_deleted boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
