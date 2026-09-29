-- Approvers passing on a pending post without rejecting it: the post
-- leaves their queue (status:unmoderated), and other approvers see why.
CREATE TABLE post_disapprovals (
    post_id bigint NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- moekura_core::moderation::DisapprovalReason.
    reason text NOT NULL CHECK (reason IN ('breaks_rules', 'poor_quality', 'disinterest')),
    message text NOT NULL DEFAULT '' CHECK (length(message) <= 2000),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (post_id, user_id)
);

CREATE INDEX post_disapprovals_user_idx ON post_disapprovals (user_id, created_at DESC);
