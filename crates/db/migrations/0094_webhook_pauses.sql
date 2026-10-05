-- Deliveries in a row that couldn't reach a webhook's receiver (or timed
-- out), and until when, after too many, deliveries wait rather than each
-- holding a job worker for the whole timeout.
ALTER TABLE webhooks
    ADD COLUMN network_failures integer NOT NULL DEFAULT 0,
    ADD COLUMN paused_until timestamptz;
