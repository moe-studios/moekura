-- Metatags in the tag box (moekura_core::post_edit).

-- A rating for mass edits to set (rating:x in the tags to add), alongside
-- their tags.
ALTER TABLE mass_updates ADD COLUMN rating text CHECK (rating IN ('g', 's', 'q', 'e'));

-- `newpool:` starts a pool now, so tag names can't start with it: tags
-- that did become `newpool_…`, or `newpool_…_(tag)` when that name is
-- taken too. Relations naming them follow.
CREATE TEMPORARY TABLE newpool_renames ON COMMIT DROP AS
SELECT t.name AS old_name,
       CASE WHEN EXISTS (SELECT 1 FROM tags o WHERE o.name = 'newpool_' || substr(t.name, 9))
            THEN 'newpool_' || substr(t.name, 9) || '_(tag)'
            ELSE 'newpool_' || substr(t.name, 9)
       END AS new_name
FROM tags t
WHERE t.name LIKE 'newpool:%';

UPDATE tags SET name = r.new_name FROM newpool_renames r WHERE tags.name = r.old_name;
UPDATE tag_relations SET antecedent_name = r.new_name
FROM newpool_renames r WHERE tag_relations.antecedent_name = r.old_name;
UPDATE tag_relations SET consequent_name = r.new_name
FROM newpool_renames r WHERE tag_relations.consequent_name = r.old_name;
