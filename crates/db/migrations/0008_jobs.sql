-- Background job queue. Finished jobs are deleted so the table stays small;
-- only jobs that are waiting, running or dead are kept.
CREATE TABLE jobs (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind text NOT NULL,
    payload jsonb NOT NULL DEFAULT '{}',
    status text NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'dead')),
    attempts integer NOT NULL DEFAULT 0,
    max_attempts integer NOT NULL DEFAULT 5 CHECK (max_attempts > 0),
    run_at timestamptz NOT NULL DEFAULT now(),
    locked_by text,
    locked_until timestamptz,
    last_error text,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- What workers claim from.
CREATE INDEX jobs_queued_idx ON jobs (run_at, id) WHERE status = 'queued';
-- Finding jobs whose worker went away.
CREATE INDEX jobs_running_idx ON jobs (locked_until) WHERE status = 'running';
