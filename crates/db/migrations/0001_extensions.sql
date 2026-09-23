-- Extensions used across the schema. All are "trusted" extensions, so the
-- database owner can create them without superuser rights.

-- Case-insensitive text for user names and emails.
CREATE EXTENSION IF NOT EXISTS citext;
-- GIN operator class for integer arrays; backs tag search on posts.tag_ids.
CREATE EXTENSION IF NOT EXISTS intarray;
-- Trigram indexes for tag autocomplete and wildcard matching.
CREATE EXTENSION IF NOT EXISTS pg_trgm;
