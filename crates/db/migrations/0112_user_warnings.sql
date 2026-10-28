-- Formal warnings staff give users, short of a ban: why, and what about
-- if it was something they wrote (a comment or forum post), which then
-- says so.
CREATE TABLE user_warnings (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    creator_id bigint REFERENCES users (id) ON DELETE SET NULL,
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 2000),
    subject_kind text CHECK (subject_kind IN ('comment', 'forum_post')),
    subject_id bigint,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((subject_kind IS NULL) = (subject_id IS NULL))
);

CREATE INDEX user_warnings_user_idx ON user_warnings (user_id, id DESC);
CREATE INDEX user_warnings_subject_idx ON user_warnings (subject_kind, subject_id)
    WHERE subject_kind IS NOT NULL;
