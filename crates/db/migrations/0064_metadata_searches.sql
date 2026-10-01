-- Searches by file metadata and Pixiv id, and embedded notes.

-- exif: searches, which are typed in lower case with underscores for
-- spaces: the metadata as they'd type it, indexed in place of the plain
-- index from 0061.
CREATE FUNCTION metadata_search(metadata jsonb) RETURNS jsonb
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
    SELECT coalesce(jsonb_object_agg(lower(key), lower(replace(value, ' ', '_'))), '{}'::jsonb)
    FROM jsonb_each_text(metadata)
$$;
DROP INDEX media_assets_metadata_idx;
CREATE INDEX media_assets_metadata_search_idx ON media_assets USING gin (metadata_search(metadata));

-- The Pixiv work a post's source links to (a work's page, the old
-- member_illust.php page, or one of its files on i.pximg.net), for
-- pixiv: and pixiv_id: searches.
ALTER TABLE posts ADD COLUMN pixiv_id bigint GENERATED ALWAYS AS (
    substring(source FROM
        '(?:pixiv\.net/(?:[a-z]{2}/)?artworks/|pixiv\.net/member_illust\.php\?.*illust_id=|pximg\.net/.*/)([0-9]{1,18})'
    )::bigint
) STORED;
CREATE INDEX posts_pixiv_id_idx ON posts (pixiv_id) WHERE pixiv_id IS NOT NULL;

-- The post's notes are drawn on the picture, text and all, rather than
-- shown when pointed at (Danbooru's embedded notes).
ALTER TABLE posts ADD COLUMN has_embedded_notes boolean NOT NULL DEFAULT false;

-- `exif:`, `embedded:`, `pixiv:` and `pixiv_id:` are search metatags now,
-- so tag names can't start with them: tags that did become `exif_…` (and
-- so on), or `exif_…_(tag)` when that name is taken too. Relations naming
-- them follow.
CREATE TEMPORARY TABLE metadata_renames ON COMMIT DROP AS
SELECT t.name AS old_name,
       CASE WHEN EXISTS (SELECT 1 FROM tags o
                         WHERE o.name = p.prefix || '_' || substr(t.name, length(p.prefix) + 2))
            THEN p.prefix || '_' || substr(t.name, length(p.prefix) + 2) || '_(tag)'
            ELSE p.prefix || '_' || substr(t.name, length(p.prefix) + 2)
       END AS new_name
FROM tags t
JOIN (VALUES ('exif'), ('embedded'), ('pixiv'), ('pixiv_id')) AS p (prefix)
  ON t.name LIKE p.prefix || ':%';

UPDATE tags SET name = r.new_name FROM metadata_renames r WHERE tags.name = r.old_name;
UPDATE tag_relations SET antecedent_name = r.new_name
FROM metadata_renames r WHERE tag_relations.antecedent_name = r.old_name;
UPDATE tag_relations SET consequent_name = r.new_name
FROM metadata_renames r WHERE tag_relations.consequent_name = r.old_name;
