-- Thumbnails are no longer cropped to squares: the chosen regions and the
-- square_thumbnails setting go. The crop-<size> renditions' files are
-- in storage, which SQL can't reach, so a media.remove_square_thumbnails
-- job removes them and their rows.
ALTER TABLE media_assets DROP COLUMN crop;

UPDATE users SET settings = settings - 'square_thumbnails' WHERE settings ? 'square_thumbnails';

INSERT INTO jobs (kind, max_attempts)
SELECT 'media.remove_square_thumbnails', 5
WHERE EXISTS (SELECT 1 FROM media_variants WHERE starts_with(kind, 'crop-'));
