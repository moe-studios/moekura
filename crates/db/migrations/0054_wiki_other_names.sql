-- Other names for wiki pages (moekura_core::wiki): what the tag is called
-- elsewhere, for the wiki search and for translating tags from sources.
ALTER TABLE wiki_pages ADD COLUMN other_names text[] NOT NULL DEFAULT '{}'
    CHECK (cardinality(other_names) <= 50);
ALTER TABLE wiki_page_versions ADD COLUMN other_names text[] NOT NULL DEFAULT '{}';

-- Pages with a given other name (other_names @> ARRAY[…]).
CREATE INDEX wiki_pages_other_names_idx ON wiki_pages USING gin (other_names);
