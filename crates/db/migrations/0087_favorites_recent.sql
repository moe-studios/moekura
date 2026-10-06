-- Everyone's favorites, newest first (`/favorites.json` without a user or
-- post), read in order instead of sorting the whole table.
CREATE INDEX favorites_recent_idx ON favorites (created_at DESC, post_id DESC);
