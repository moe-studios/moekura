-- `source:` searches match prefixes and wildcards, regardless of case.
CREATE INDEX posts_source_trgm_idx ON posts USING gin (source gin_trgm_ops) WHERE source <> '';
