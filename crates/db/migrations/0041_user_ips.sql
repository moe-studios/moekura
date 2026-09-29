-- The addresses each account used, for staff looking for ban evasion:
-- recorded on login and (at most hourly) on changes, and pruned after
-- the ip_history_days site setting.
CREATE TABLE user_ips (
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    ip inet NOT NULL,
    first_seen_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, ip)
);

-- Other accounts on the same address.
CREATE INDEX user_ips_ip_idx ON user_ips (ip, user_id);
CREATE INDEX user_ips_last_seen_at_idx ON user_ips (last_seen_at);
