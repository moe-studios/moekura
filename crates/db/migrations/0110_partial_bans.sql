-- A ban can take away only some of what a user may do (commenting, say,
-- or uploading): the permission bits it takes, as roles.permissions has
-- them. NULL is a full ban, leaving only looking around.
ALTER TABLE bans ADD COLUMN restricts bigint;
