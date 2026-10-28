-- A deleted post whose files are being removed for good. From then on it
-- can't be restored or appealed: the purge job deletes the row once the
-- files are gone, but only while the post is still deleted.
ALTER TABLE posts ADD COLUMN purging boolean NOT NULL DEFAULT false;
