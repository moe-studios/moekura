-- The moderation audit log: every moderation and admin action, who took
-- it and why. Targets are plain ids, not foreign keys, so entries outlive
-- purged posts.
CREATE TABLE mod_actions (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- NULL for actions from the command line.
    actor_id bigint REFERENCES users (id) ON DELETE SET NULL,
    -- uwu_core::moderation::ActionKind.
    action text NOT NULL,
    post_id bigint,
    user_id bigint,
    reason text NOT NULL DEFAULT '' CHECK (length(reason) <= 2000),
    details jsonb NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX mod_actions_action_idx ON mod_actions (action, id DESC);
CREATE INDEX mod_actions_actor_idx ON mod_actions (actor_id, id DESC);
CREATE INDEX mod_actions_post_idx ON mod_actions (post_id, id DESC) WHERE post_id IS NOT NULL;
CREATE INDEX mod_actions_user_idx ON mod_actions (user_id, id DESC) WHERE user_id IS NOT NULL;
