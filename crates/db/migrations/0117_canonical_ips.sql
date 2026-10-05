-- Client addresses are now always canonical: behind a dual-stack listener
-- an IPv4 client is 192.0.2.1, no longer ::ffff:192.0.2.1. Bans and
-- address history recorded the old way become the IPv4 they stand for,
-- so they keep matching. (Wider IPv6 ranges that merely contain mapped
-- addresses, such as ::/64, are left alone: they no longer cover IPv4
-- clients.)
UPDATE ip_bans
SET network = set_masklen('0.0.0.0'::inet + (network - '::ffff:0.0.0.0'::inet),
                          masklen(network) - 96)::cidr
WHERE network <<= '::ffff:0.0.0.0/96' AND masklen(network) >= 96;

INSERT INTO user_ips (user_id, ip, first_seen_at, last_seen_at)
SELECT user_id, '0.0.0.0'::inet + (ip - '::ffff:0.0.0.0'::inet), first_seen_at, last_seen_at
FROM user_ips
WHERE ip <<= '::ffff:0.0.0.0/96'
ON CONFLICT (user_id, ip) DO UPDATE
SET first_seen_at = least(user_ips.first_seen_at, EXCLUDED.first_seen_at),
    last_seen_at = greatest(user_ips.last_seen_at, EXCLUDED.last_seen_at);
DELETE FROM user_ips WHERE ip <<= '::ffff:0.0.0.0/96';

UPDATE sessions SET ip = '0.0.0.0'::inet + (ip - '::ffff:0.0.0.0'::inet)
WHERE ip <<= '::ffff:0.0.0.0/96';
