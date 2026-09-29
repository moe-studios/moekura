-- Tags per category on each post, for gentags:, arttags: and the like:
-- element id + 1 counts the tags of category `id` (arrays start at 1).
-- Categories without an element have none.
CREATE FUNCTION post_category_counts(tag_ids int4[]) RETURNS int4[]
LANGUAGE sql STABLE AS $$
    SELECT coalesce(array_agg(coalesce(counts.n, 0) ORDER BY g), '{}')
    FROM generate_series(0, (SELECT max(c.id) FROM tag_categories c)) AS g
    LEFT JOIN (
        SELECT t.category_id, count(*)::int AS n
        FROM tags t WHERE t.id = ANY(tag_ids)
        GROUP BY t.category_id
    ) AS counts ON counts.category_id = g
$$;

ALTER TABLE posts ADD COLUMN category_counts int4[] NOT NULL DEFAULT '{}';

UPDATE posts SET category_counts = post_category_counts(tag_ids) WHERE tag_ids <> '{}';

CREATE FUNCTION posts_count_categories() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.category_counts := post_category_counts(NEW.tag_ids);
    RETURN NEW;
END
$$;

CREATE TRIGGER posts_count_categories
    BEFORE INSERT OR UPDATE OF tag_ids ON posts
    FOR EACH ROW EXECUTE FUNCTION posts_count_categories();

-- A tag moving to another category changes the counts of its posts.
CREATE FUNCTION tags_recount_categories() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE posts SET category_counts = post_category_counts(tag_ids)
    WHERE tag_ids @> ARRAY[NEW.id];
    RETURN NULL;
END
$$;

CREATE TRIGGER tags_recount_categories
    AFTER UPDATE OF category_id ON tags
    FOR EACH ROW WHEN (OLD.category_id IS DISTINCT FROM NEW.category_id)
    EXECUTE FUNCTION tags_recount_categories();
