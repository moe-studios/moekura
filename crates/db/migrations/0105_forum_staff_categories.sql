-- Categories only forum staff start topics in or move topics to, such as
-- the staff's announcements; anyone may still reply.
ALTER TABLE forum_categories ADD COLUMN staff_only boolean NOT NULL DEFAULT false;

UPDATE forum_categories SET staff_only = true WHERE name = 'Site news';
