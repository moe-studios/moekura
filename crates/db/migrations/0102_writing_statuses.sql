-- One status for comments, forum posts and messages, in place of a flag
-- that meant deleted by the writer, hidden by staff and held as spam
-- alike. Restoring something hidden no longer lets held spam out or
-- undoes its writer's own deletion, and a rejected message is kept. The
-- old flags stay, worked out from the status.

-- Comments: deleted by their writer, hidden by staff, or held as spam.
-- Which of the deleted ones staff hid shows in the log.
ALTER TABLE comments ADD COLUMN status text NOT NULL DEFAULT 'active'
    CHECK (status IN ('active', 'deleted', 'hidden', 'held'));
WITH last AS (
    SELECT DISTINCT ON (comment_id) comment_id, action
    FROM (SELECT id, action,
                 CASE WHEN action = 'held.reject' THEN (details->>'id')::bigint
                      ELSE (details->>'comment_id')::bigint END AS comment_id
          FROM mod_actions
          WHERE action IN ('comment.hide', 'comment.restore')
             OR (action = 'held.reject' AND details->>'kind' = 'comment')) logged
    WHERE comment_id IS NOT NULL
    ORDER BY comment_id, id DESC)
UPDATE comments c SET status = CASE
        WHEN c.held_reason IS NOT NULL THEN 'held'
        WHEN last.action IN ('comment.hide', 'held.reject') THEN 'hidden'
        ELSE 'deleted' END
FROM comments c2 LEFT JOIN last ON last.comment_id = c2.id
WHERE c2.id = c.id AND c.is_deleted;

DROP TRIGGER comments_count ON comments;
ALTER TABLE comments DROP COLUMN is_deleted;
ALTER TABLE comments ADD COLUMN is_deleted boolean
    GENERATED ALWAYS AS (status <> 'active') STORED;
CREATE INDEX comments_recent_idx ON comments (id DESC) WHERE NOT is_deleted;
CREATE INDEX comments_body_idx ON comments USING gin (to_tsvector('simple', body))
    WHERE NOT is_deleted;
CREATE TRIGGER comments_count
    AFTER INSERT OR DELETE OR UPDATE OF status ON comments
    FOR EACH ROW EXECUTE FUNCTION comments_count();
DROP INDEX comments_held_idx;
CREATE INDEX comments_held_idx ON comments (created_at) WHERE status = 'held';

-- Forum posts: hidden by staff, or held as spam (writers can't delete
-- theirs yet).
ALTER TABLE forum_posts ADD COLUMN status text NOT NULL DEFAULT 'active'
    CHECK (status IN ('active', 'deleted', 'hidden', 'held'));
UPDATE forum_posts SET status = CASE WHEN held_reason IS NOT NULL THEN 'held' ELSE 'hidden' END
WHERE is_hidden;

DROP TRIGGER forum_posts_counts ON forum_posts;
ALTER TABLE forum_posts DROP COLUMN is_hidden;
ALTER TABLE forum_posts ADD COLUMN is_hidden boolean
    GENERATED ALWAYS AS (status <> 'active') STORED;
CREATE INDEX forum_posts_body_idx ON forum_posts USING gin (to_tsvector('simple', body))
    WHERE NOT is_hidden;
CREATE TRIGGER forum_posts_counts
    AFTER INSERT OR DELETE OR UPDATE OF topic_id, status ON forum_posts
    FOR EACH ROW EXECUTE FUNCTION forum_topic_counts();
DROP INDEX forum_posts_held_idx;
CREATE INDEX forum_posts_held_idx ON forum_posts (created_at) WHERE status = 'held';

-- Messages: each copy deleted by its owner, held as spam, or rejected
-- (kept, never delivered).
ALTER TABLE dmails ADD COLUMN status text NOT NULL DEFAULT 'active'
    CHECK (status IN ('active', 'deleted', 'held', 'rejected'));
UPDATE dmails SET status = CASE WHEN held_reason IS NOT NULL THEN 'held' ELSE 'deleted' END
WHERE is_deleted;

ALTER TABLE dmails DROP COLUMN is_deleted;
ALTER TABLE dmails ADD COLUMN is_deleted boolean
    GENERATED ALWAYS AS (status <> 'active') STORED;
CREATE INDEX dmails_owner_idx ON dmails (owner_id, id DESC) WHERE NOT is_deleted;
CREATE INDEX dmails_unread_idx ON dmails (owner_id) WHERE NOT is_read AND NOT is_deleted;
DROP INDEX dmails_held_idx;
CREATE INDEX dmails_held_idx ON dmails (created_at) WHERE status = 'held';
