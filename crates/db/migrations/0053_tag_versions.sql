-- Tag history: a snapshot of a tag's name, category and deprecation when
-- it's created and after every change to them, credited to whoever
-- post_versions_updater() names (moekura_db::post_versions::attribute).
CREATE TABLE tag_versions (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tag_id integer NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    -- 1 for the tag's creation, then counting up per tag.
    version integer NOT NULL,
    updater_id bigint REFERENCES users (id) ON DELETE SET NULL,
    name text NOT NULL,
    category_id smallint NOT NULL,
    is_deprecated boolean NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tag_id, version)
);

CREATE INDEX tag_versions_updater_id_idx ON tag_versions (updater_id, id DESC)
    WHERE updater_id IS NOT NULL;

-- Uploads create tags in batches, so creation is recorded per statement.
CREATE FUNCTION tags_record_inserted() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO tag_versions (tag_id, version, updater_id, name, category_id, is_deprecated)
    SELECT n.id, 1, post_versions_updater(), n.name, n.category_id, n.is_deprecated
    FROM new_rows n;
    RETURN NULL;
END
$$;

CREATE FUNCTION tags_record_updated() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO tag_versions (tag_id, version, updater_id, name, category_id, is_deprecated)
    SELECT NEW.id,
           coalesce((SELECT max(v.version) FROM tag_versions v WHERE v.tag_id = NEW.id), 0) + 1,
           post_versions_updater(), NEW.name, NEW.category_id, NEW.is_deprecated;
    RETURN NULL;
END
$$;

CREATE TRIGGER tags_versions_insert
    AFTER INSERT ON tags REFERENCING NEW TABLE AS new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION tags_record_inserted();
-- Per row with a condition, not per statement: every post edit updates
-- post counts, and the condition skips those without calling anything.
CREATE TRIGGER tags_versions_update
    AFTER UPDATE OF name, category_id, is_deprecated ON tags
    FOR EACH ROW
    WHEN ((OLD.name, OLD.category_id, OLD.is_deprecated)
          IS DISTINCT FROM (NEW.name, NEW.category_id, NEW.is_deprecated))
    EXECUTE FUNCTION tags_record_updated();

-- Existing tags start their history here.
INSERT INTO tag_versions (tag_id, version, name, category_id, is_deprecated, created_at)
SELECT id, 1, name, category_id, is_deprecated, created_at FROM tags;
