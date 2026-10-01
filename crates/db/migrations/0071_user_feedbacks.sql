-- Feedback staff and senior users leave on a user: positive, neutral or
-- negative, shown on their profile and weighed in automatic promotion.
CREATE TABLE user_feedbacks (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    creator_id bigint REFERENCES users (id) ON DELETE SET NULL,
    category text NOT NULL CHECK (category IN ('positive', 'neutral', 'negative')),
    -- moekura_core::markup.
    body text NOT NULL CHECK (length(body) BETWEEN 1 AND 20000),
    -- Deleted by staff (still seen by them).
    is_deleted boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX user_feedbacks_user_idx ON user_feedbacks (user_id, id DESC);
CREATE INDEX user_feedbacks_creator_idx ON user_feedbacks (creator_id, id DESC)
    WHERE creator_id IS NOT NULL;

ALTER TABLE notifications DROP CONSTRAINT notifications_kind_check;
ALTER TABLE notifications ADD CONSTRAINT notifications_kind_check
    CHECK (kind IN ('mention', 'reply', 'message', 'forum', 'request', 'feedback'));

-- The new give_feedback permission (bit 26) for contributors and up, as
-- SystemRole::default_permissions has it.
UPDATE roles SET permissions = permissions | (1::bigint << 26)
WHERE system_key IN ('contributor', 'janitor', 'moderator');
