-- Artist URLs are now kept with quotes, angle brackets and spaces
-- percent-encoded: canonical profile forms were built from decoded path
-- segments, so a link typed with %22 or %3C was stored with the raw
-- character. An artists.normalize_urls job encodes the stored ones and
-- recomputes their comparison forms.
INSERT INTO jobs (kind, max_attempts)
SELECT 'artists.normalize_urls', 5
WHERE EXISTS (SELECT 1 FROM artist_urls);
