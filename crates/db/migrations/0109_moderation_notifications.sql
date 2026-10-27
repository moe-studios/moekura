-- Notifications about what staff did to what one posted: a post rejected,
-- deleted or restored, an appeal decided, a comment or forum post hidden
-- or restored, writing held for review let through or turned away, a
-- warning, a ban appeal decided.
ALTER TABLE notifications DROP CONSTRAINT notifications_kind_check;
ALTER TABLE notifications ADD CONSTRAINT notifications_kind_check
    CHECK (kind IN ('mention', 'reply', 'message', 'forum', 'request', 'feedback', 'moderation'));
