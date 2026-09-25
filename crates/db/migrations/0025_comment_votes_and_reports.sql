-- Votes on comments; comments.score follows them exactly.
ALTER TABLE comments ADD COLUMN score integer NOT NULL DEFAULT 0;

CREATE TABLE comment_votes (
    comment_id bigint NOT NULL REFERENCES comments (id) ON DELETE CASCADE,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    score smallint NOT NULL CHECK (score IN (-1, 1)),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (comment_id, user_id)
);

CREATE INDEX comment_votes_user_id_idx ON comment_votes (user_id, created_at DESC);

CREATE FUNCTION comment_votes_score() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') THEN
        UPDATE comments SET score = score - OLD.score WHERE id = OLD.comment_id;
    END IF;
    IF TG_OP IN ('INSERT', 'UPDATE') THEN
        UPDATE comments SET score = score + NEW.score WHERE id = NEW.comment_id;
    END IF;
    RETURN NULL;
END
$$;

CREATE TRIGGER comment_votes_score
    AFTER INSERT OR UPDATE OR DELETE ON comment_votes
    FOR EACH ROW EXECUTE FUNCTION comment_votes_score();

-- Reports that a comment breaks the rules, like post_flags. Staff settle
-- them by hiding the comment (upheld) or dismissing them.
CREATE TABLE comment_reports (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    comment_id bigint NOT NULL REFERENCES comments (id) ON DELETE CASCADE,
    creator_id bigint REFERENCES users (id) ON DELETE SET NULL,
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 2000),
    status text NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'dismissed', 'upheld')),
    resolver_id bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    resolved_at timestamptz
);

-- One open report per person and comment.
CREATE UNIQUE INDEX comment_reports_open_idx ON comment_reports (comment_id, creator_id)
    WHERE status = 'open';
CREATE INDEX comment_reports_queue_idx ON comment_reports (id) WHERE status = 'open';

-- The new moderate_comments permission (bit 18) for the built-in staff
-- roles, as SystemRole::default_permissions has it.
UPDATE roles SET permissions = permissions | (1::bigint << 18)
WHERE system_key IN ('janitor', 'moderator');
