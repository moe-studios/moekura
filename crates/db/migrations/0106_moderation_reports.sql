-- Reports to staff about messages, forum posts, users, wiki pages, pools,
-- notes, artists and feedback (posts have flags, comments their own
-- reports). Each report is its own row with its reason and the reported
-- text as it was, so an edit can't clean up the evidence and a second
-- report can't overwrite the first; settling records how it ended.
CREATE TABLE moderation_reports (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- moekura_core::moderation::ReportKind, and the thing's id.
    subject_kind text NOT NULL,
    subject_id bigint NOT NULL,
    -- Who wrote or made it, if anyone (for rank checks and records).
    owner_id bigint REFERENCES users (id) ON DELETE SET NULL,
    creator_id bigint REFERENCES users (id) ON DELETE SET NULL,
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 2000),
    snapshot text NOT NULL DEFAULT '' CHECK (length(snapshot) <= 50000),
    status text NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'dismissed', 'upheld')),
    resolver_id bigint REFERENCES users (id) ON DELETE SET NULL,
    outcome text NOT NULL DEFAULT '' CHECK (length(outcome) <= 2000),
    created_at timestamptz NOT NULL DEFAULT now(),
    resolved_at timestamptz
);

-- One open report per person and thing.
CREATE UNIQUE INDEX moderation_reports_open_idx
    ON moderation_reports (subject_kind, subject_id, creator_id) WHERE status = 'open';
CREATE INDEX moderation_reports_queue_idx ON moderation_reports (subject_kind, id)
    WHERE status = 'open';
CREATE INDEX moderation_reports_subject_idx ON moderation_reports (subject_kind, subject_id, id);
CREATE INDEX moderation_reports_owner_idx ON moderation_reports (owner_id, id DESC)
    WHERE owner_id IS NOT NULL;

-- Reports about messages move here; settled ones were dismissed.
INSERT INTO moderation_reports (subject_kind, subject_id, owner_id, creator_id, reason,
                                snapshot, status, created_at, resolved_at)
SELECT 'dmail', d.id, d.from_id, d.owner_id,
       coalesce(nullif(btrim(d.report_reason), ''), '(no reason given)'),
       left(d.title || E'\n\n' || d.body, 50000),
       CASE WHEN d.report_settled THEN 'dismissed' ELSE 'open' END,
       d.reported_at, CASE WHEN d.report_settled THEN d.reported_at END
FROM dmails d WHERE d.reported_at IS NOT NULL;

DROP INDEX dmails_reported_idx;
ALTER TABLE dmails
    DROP COLUMN reported_at,
    DROP COLUMN report_reason,
    DROP COLUMN report_settled;

-- Comment reports keep the comment as it was too.
ALTER TABLE comment_reports ADD COLUMN snapshot text NOT NULL DEFAULT ''
    CHECK (length(snapshot) <= 50000);

-- The new moderate_messages permission (bit 28) for moderators, as
-- SystemRole::default_permissions has it: reported and held messages are
-- no longer for everyone who hides comments.
UPDATE roles SET permissions = permissions | (1::bigint << 28) WHERE system_key = 'moderator';
