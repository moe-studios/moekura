-- A post's earlier file, kept after a replacement, can be removed for good
-- (say it showed what the replacement took out): the file goes, and the
-- replacement keeps who removed it and when.
ALTER TABLE post_replacements
    ADD COLUMN old_removed_at timestamptz,
    ADD COLUMN old_remover_id bigint REFERENCES users (id) ON DELETE SET NULL;
