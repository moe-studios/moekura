-- The region square thumbnails (crop-<size> renditions) show: left, top
-- and side, in the file's pixels. None: the part libvips finds most
-- interesting.
ALTER TABLE media_assets ADD COLUMN crop integer[]
    CHECK (crop IS NULL OR (cardinality(crop) = 3 AND crop[1] >= 0 AND crop[2] >= 0 AND crop[3] > 0));
