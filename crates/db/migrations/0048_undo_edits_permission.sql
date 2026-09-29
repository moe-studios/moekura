-- The new undo_edits permission (bit 23) for moderators, as
-- SystemRole::default_permissions has it. Undoing a user's edits needed
-- mass_edit_tags before it had a permission of its own.
UPDATE roles SET permissions = permissions | (1::bigint << 23) WHERE system_key = 'moderator';
