-- The status a deleted post had. Restoring a post deleted while it waited
-- for approval (a rejected one, say) sends it back to the queue instead
-- of letting it in, so deleting and restoring can't approve anything.
ALTER TABLE posts ADD COLUMN deleted_from text
    CHECK (deleted_from IN ('pending', 'active', 'flagged'));

-- Before this, rejections are what the log tells apart.
UPDATE posts p SET deleted_from = 'pending'
FROM (SELECT DISTINCT ON (post_id) post_id, action FROM mod_actions
      WHERE action IN ('post.reject', 'post.delete', 'post.restore') AND post_id IS NOT NULL
      ORDER BY post_id, id DESC) last
WHERE last.post_id = p.id AND last.action = 'post.reject' AND p.status = 'deleted';
