-- Upload limits per role. NULL: no limit.
ALTER TABLE roles
    -- Uploads waiting in the approval queue at once.
    ADD COLUMN pending_upload_limit integer CHECK (pending_upload_limit >= 0),
    -- Uploads in the last 24 hours.
    ADD COLUMN daily_upload_limit integer CHECK (daily_upload_limit >= 0);

-- Members start with Danbooru's base limit of 10 pending uploads; it
-- only matters when the approval queue is on.
UPDATE roles SET pending_upload_limit = 10 WHERE system_key = 'member';

-- Counting a user's uploads of the last day.
CREATE INDEX posts_uploader_created_at_idx ON posts (uploader_id, created_at DESC)
    WHERE uploader_id IS NOT NULL;
