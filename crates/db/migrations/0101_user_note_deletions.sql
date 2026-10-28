-- Deleting a staff note keeps it, with who deleted it and when, so staff
-- above them can still read it and the log has a trace.
ALTER TABLE user_notes
    ADD COLUMN deleted_at timestamptz,
    ADD COLUMN deleter_id bigint REFERENCES users (id) ON DELETE SET NULL;
