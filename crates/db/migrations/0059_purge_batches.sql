-- Purging deleted posts in bulk: those ticked, or every deleted post a
-- search finds.
ALTER TABLE post_batches DROP CONSTRAINT post_batches_kind_check;
ALTER TABLE post_batches ADD CONSTRAINT post_batches_kind_check CHECK (kind IN ('delete', 'purge'));

-- The search, with `status:deleted` in it.
ALTER TABLE post_batches ADD COLUMN query text CHECK (length(query) <= 1000);
-- The posts ticked, newest first.
ALTER TABLE post_batches ADD COLUMN post_ids bigint[];
