-- What an entry is about besides a post or a user (a comment, a forum post
-- or topic, a message, …), in columns the log can be filtered by rather
-- than only in its details.
ALTER TABLE mod_actions
    ADD COLUMN subject_kind text,
    ADD COLUMN subject_id bigint,
    ADD CHECK ((subject_kind IS NULL) = (subject_id IS NULL));

UPDATE mod_actions SET subject_kind = 'comment', subject_id = (details->>'comment_id')::bigint
WHERE details ? 'comment_id';
UPDATE mod_actions SET subject_kind = 'forum_post', subject_id = (details->>'forum_post_id')::bigint
WHERE details ? 'forum_post_id';
UPDATE mod_actions SET subject_kind = 'forum_topic', subject_id = (details->>'topic_id')::bigint
WHERE details ? 'topic_id' AND NOT details ? 'forum_post_id';
UPDATE mod_actions SET subject_kind = 'forum_topic', subject_id = (details->>'from')::bigint
WHERE action = 'forum_topic.merge';
UPDATE mod_actions SET subject_kind = 'dmail', subject_id = (details->>'dmail_id')::bigint
WHERE details ? 'dmail_id';
UPDATE mod_actions SET subject_kind = details->>'kind', subject_id = (details->>'id')::bigint
WHERE action IN ('held.approve', 'held.reject');
UPDATE mod_actions SET subject_kind = 'user_note', subject_id = (details->>'note')::bigint
WHERE action = 'user_note.delete';
UPDATE mod_actions SET subject_kind = 'user_feedback', subject_id = (details->>'feedback_id')::bigint
WHERE details ? 'feedback_id';
UPDATE mod_actions SET subject_kind = 'pool', subject_id = (details->>'pool_id')::bigint
WHERE details ? 'pool_id';
UPDATE mod_actions SET subject_kind = 'news', subject_id = (details->>'news_id')::bigint
WHERE details ? 'news_id';

CREATE INDEX mod_actions_subject_idx ON mod_actions (subject_kind, subject_id, id DESC)
    WHERE subject_kind IS NOT NULL;
