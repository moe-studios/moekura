-- order:change, and updated: searches.
CREATE INDEX posts_updated_at_idx ON posts (updated_at DESC, id DESC);
