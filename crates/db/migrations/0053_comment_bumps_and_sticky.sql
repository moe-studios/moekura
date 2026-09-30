-- Comments can be posted without bumping their post in
-- order:comment_bumped, and staff can pin (sticky) a comment to the top
-- of its post's comments.
ALTER TABLE comments
    ADD COLUMN do_not_bump boolean NOT NULL DEFAULT false,
    ADD COLUMN is_sticky boolean NOT NULL DEFAULT false;

ALTER TABLE posts ADD COLUMN last_comment_bumped_at timestamptz;

UPDATE posts SET last_comment_bumped_at = last_commented_at
WHERE last_commented_at IS NOT NULL;

-- order:comment_bumped.
CREATE INDEX posts_last_comment_bumped_at_idx ON posts (last_comment_bumped_at DESC, id DESC)
    WHERE last_comment_bumped_at IS NOT NULL;

-- As in 0024, plus last_comment_bumped_at, which follows the visible
-- comments that bump.
CREATE OR REPLACE FUNCTION comments_count() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    change integer := 0;
    post bigint;
BEGIN
    IF TG_OP = 'INSERT' THEN
        post := NEW.post_id;
        IF NOT NEW.is_deleted THEN
            UPDATE posts SET comment_count = comment_count + 1,
                             last_commented_at = GREATEST(last_commented_at, NEW.created_at),
                             last_comment_bumped_at = CASE WHEN NEW.do_not_bump
                                 THEN last_comment_bumped_at
                                 ELSE GREATEST(last_comment_bumped_at, NEW.created_at) END
            WHERE id = post;
        END IF;
        RETURN NULL;
    ELSIF TG_OP = 'DELETE' THEN
        post := OLD.post_id;
        change := CASE WHEN OLD.is_deleted THEN 0 ELSE -1 END;
    ELSE
        post := NEW.post_id;
        change := (CASE WHEN NEW.is_deleted THEN 0 ELSE 1 END)
                - (CASE WHEN OLD.is_deleted THEN 0 ELSE 1 END);
    END IF;
    IF change <> 0 THEN
        UPDATE posts SET comment_count = comment_count + change,
                         last_commented_at = (SELECT max(c.created_at) FROM comments c
                                              WHERE c.post_id = post AND NOT c.is_deleted),
                         last_comment_bumped_at = (SELECT max(c.created_at) FROM comments c
                                                   WHERE c.post_id = post AND NOT c.is_deleted
                                                     AND NOT c.do_not_bump)
        WHERE id = post;
    END IF;
    RETURN NULL;
END
$$;
