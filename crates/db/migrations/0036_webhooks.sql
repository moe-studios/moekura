-- Outgoing webhooks: URLs told about events on the site
-- (moekura_core::webhooks), and every delivery to them.
CREATE TABLE webhooks (
    id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    url text NOT NULL CHECK (length(url) BETWEEN 1 AND 2048),
    description text NOT NULL DEFAULT '' CHECK (length(description) <= 200),
    -- Signs deliveries; shown to admins so receivers can check them.
    secret text NOT NULL,
    events text[] NOT NULL DEFAULT '{}',
    is_enabled boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE webhook_deliveries (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    webhook_id integer NOT NULL REFERENCES webhooks (id) ON DELETE CASCADE,
    event text NOT NULL,
    payload jsonb NOT NULL,
    status text NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'delivered', 'failed')),
    attempts integer NOT NULL DEFAULT 0,
    response_status integer,
    -- The start of the response, or why there was none.
    response text,
    created_at timestamptz NOT NULL DEFAULT now(),
    delivered_at timestamptz
);

CREATE INDEX webhook_deliveries_log_idx ON webhook_deliveries (webhook_id, id DESC);
-- Deliveries older than a month are removed by the scheduled cleanup.
CREATE INDEX webhook_deliveries_created_at_idx ON webhook_deliveries (created_at);
