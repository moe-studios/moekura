-- Writing held for review as likely spam: hidden (comments and messages
-- deleted, forum posts hidden) until the staff approve it, with why.
ALTER TABLE comments ADD COLUMN held_reason text;
ALTER TABLE forum_posts ADD COLUMN held_reason text;
ALTER TABLE dmails ADD COLUMN held_reason text;
-- Deleted only because its opening post is held.
ALTER TABLE forum_topics ADD COLUMN is_held boolean NOT NULL DEFAULT false;

CREATE INDEX comments_held_idx ON comments (created_at) WHERE held_reason IS NOT NULL;
CREATE INDEX forum_posts_held_idx ON forum_posts (created_at) WHERE held_reason IS NOT NULL;
CREATE INDEX dmails_held_idx ON dmails (created_at) WHERE held_reason IS NOT NULL;
