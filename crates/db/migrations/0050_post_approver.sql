-- Who let a pending post in, for approver: searches. Posts approved
-- before this migration take it from the moderation log.
ALTER TABLE posts ADD COLUMN approver_id bigint REFERENCES users (id) ON DELETE SET NULL;

UPDATE posts p SET approver_id = a.actor_id
FROM (
    SELECT DISTINCT ON (post_id) post_id, actor_id
    FROM mod_actions
    WHERE action = 'post.approve' AND post_id IS NOT NULL
    ORDER BY post_id, id DESC
) a
WHERE a.post_id = p.id;

CREATE INDEX posts_approver_id_idx ON posts (approver_id, id DESC) WHERE approver_id IS NOT NULL;

-- comment:<text>, like note:<text>.
CREATE INDEX comments_body_idx ON comments USING gin (to_tsvector('simple', body)) WHERE NOT is_deleted;
