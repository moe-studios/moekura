-- `ai:` is a search metatag now (tags the tagger suggests), so tag names
-- can't start with it: tags that did become `ai_…`, or `ai_…_(tag)` when
-- that name is taken too. Relations naming them follow.
CREATE TEMPORARY TABLE ai_renames ON COMMIT DROP AS
SELECT t.name AS old_name,
       CASE WHEN EXISTS (SELECT 1 FROM tags o WHERE o.name = 'ai_' || substr(t.name, 4))
            THEN 'ai_' || substr(t.name, 4) || '_(tag)'
            ELSE 'ai_' || substr(t.name, 4)
       END AS new_name
FROM tags t
WHERE t.name LIKE 'ai:%';

UPDATE tags SET name = r.new_name FROM ai_renames r WHERE tags.name = r.old_name;
UPDATE tag_relations SET antecedent_name = r.new_name
FROM ai_renames r WHERE tag_relations.antecedent_name = r.old_name;
UPDATE tag_relations SET consequent_name = r.new_name
FROM ai_renames r WHERE tag_relations.consequent_name = r.old_name;
