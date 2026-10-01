-- Artist commentary: the title and description a post's artist gave it
-- where it was first posted, and their translations, with history.
CREATE TABLE artist_commentaries (
    post_id bigint PRIMARY KEY REFERENCES posts (id) ON DELETE CASCADE,
    original_title text NOT NULL DEFAULT '' CHECK (length(original_title) <= 1000),
    original_description text NOT NULL DEFAULT '' CHECK (length(original_description) <= 50000),
    translated_title text NOT NULL DEFAULT '' CHECK (length(translated_title) <= 1000),
    translated_description text NOT NULL DEFAULT '' CHECK (length(translated_description) <= 50000),
    -- The latest entry in artist_commentary_versions.
    version integer NOT NULL DEFAULT 1,
    updater_id bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX artist_commentaries_updated_at_idx ON artist_commentaries (updated_at DESC, post_id DESC);
-- commentary:<words>, like comment:<words>.
CREATE INDEX artist_commentaries_text_idx ON artist_commentaries USING gin (
    to_tsvector('simple', original_title || ' ' || original_description || ' '
                          || translated_title || ' ' || translated_description));

CREATE TABLE artist_commentary_versions (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    post_id bigint NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    version integer NOT NULL,
    updater_id bigint REFERENCES users (id) ON DELETE SET NULL,
    original_title text NOT NULL,
    original_description text NOT NULL,
    translated_title text NOT NULL,
    translated_description text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (post_id, version)
);

CREATE INDEX artist_commentary_versions_updater_idx
    ON artist_commentary_versions (updater_id, id DESC) WHERE updater_id IS NOT NULL;

-- `commentary:` is a search metatag now, so tag names can't start with
-- it: tags that did become `commentary_…`, or `commentary_…_(tag)` when
-- that name is taken too. Relations naming them follow.
CREATE TEMPORARY TABLE commentary_renames ON COMMIT DROP AS
SELECT t.name AS old_name,
       CASE WHEN EXISTS (SELECT 1 FROM tags o WHERE o.name = 'commentary_' || substr(t.name, 12))
            THEN 'commentary_' || substr(t.name, 12) || '_(tag)'
            ELSE 'commentary_' || substr(t.name, 12)
       END AS new_name
FROM tags t
WHERE t.name LIKE 'commentary:%';

UPDATE tags SET name = r.new_name FROM commentary_renames r WHERE tags.name = r.old_name;
UPDATE tag_relations SET antecedent_name = r.new_name
FROM commentary_renames r WHERE tag_relations.antecedent_name = r.old_name;
UPDATE tag_relations SET consequent_name = r.new_name
FROM commentary_renames r WHERE tag_relations.consequent_name = r.old_name;
