-- The sitewide post history filters by tag added or removed.
CREATE INDEX post_versions_added_idx ON post_versions USING gin (added_tag_ids gin__int_ops);
CREATE INDEX post_versions_removed_idx ON post_versions USING gin (removed_tag_ids gin__int_ops);
