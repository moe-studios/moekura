-- Who deleted a piece of feedback, so that only they or someone above
-- them restores it. Feedback already deleted takes its deleter from the
-- moderation log.
ALTER TABLE user_feedbacks
    ADD COLUMN deleted_by_id bigint REFERENCES users (id) ON DELETE SET NULL;

UPDATE user_feedbacks f SET deleted_by_id = (
    SELECT m.actor_id FROM mod_actions m
    WHERE m.user_id = f.user_id
      AND m.action = 'user_feedback.delete'
      AND m.details->>'feedback_id' = f.id::text
    ORDER BY m.id DESC LIMIT 1
)
WHERE f.is_deleted;
