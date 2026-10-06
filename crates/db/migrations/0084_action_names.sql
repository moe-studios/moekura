-- Audit log action names made consistent before 1.0: the same verb for
-- the same kind of action, and the singular thing acted on.
UPDATE mod_actions SET action = CASE action
    WHEN 'pool.undelete' THEN 'pool.restore'
    WHEN 'ip.ban' THEN 'network.ban'
    WHEN 'ip.unban' THEN 'network.unban'
    WHEN 'tags.mass_update' THEN 'tag.mass_update'
    WHEN 'posts.delete_uploads' THEN 'user.delete_uploads'
    WHEN 'posts.purge_batch' THEN 'post.purge_batch'
    WHEN 'post_versions.undo' THEN 'user.undo_edits'
    WHEN 'forum_post.unhide' THEN 'forum_post.restore'
END
WHERE action IN ('pool.undelete', 'ip.ban', 'ip.unban', 'tags.mass_update', 'posts.delete_uploads', 'posts.purge_batch', 'post_versions.undo', 'forum_post.unhide');
