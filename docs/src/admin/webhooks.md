# Webhooks

Webhooks tell other services about events on the site as they happen: a
chat bot announcing uploads, a mirror, or your own scripts. Add them
under **Admin → Webhooks** (for those who manage site settings), with
the URL to send to and the events it wants:

| Event | When |
|---|---|
| `post.created` | a post is uploaded (by any means) |
| `post.approved` | a pending post is approved |
| `post.deleted` | a post is deleted or rejected |
| `post.flagged` | a post is flagged |
| `comment.created` | a comment is posted |
| `user.registered` | someone registers (with a form or single sign-on) |

## Deliveries

Each event is a `POST` of JSON:

```json
{
  "id": 42,
  "event": "post.created",
  "created_at": "2026-09-26T12:00:00Z",
  "data": { "post_id": 123, "url": "https://booru.example.com/posts/123", "tags": ["cat"], … }
}
```

with these headers:

| Header | |
|---|---|
| `X-Moekura-Event` | the event |
| `X-Moekura-Delivery` | the delivery's number (the same across retries) |
| `X-Moekura-Timestamp` | seconds since 1970 when it was sent |
| `X-Moekura-Signature` | `sha256=` and the HMAC-SHA256, in hex, of `{timestamp}.{body}` with the webhook's secret |

Check the signature and reject old timestamps (older than five minutes,
say), so nobody else can send you events or replay old ones. In Python:

```python
import hashlib, hmac, time

def valid(secret: str, timestamp: str, body: bytes, signature: str) -> bool:
    expected = hmac.new(secret.encode(), timestamp.encode() + b"." + body, hashlib.sha256).hexdigest()
    fresh = abs(time.time() - int(timestamp)) < 300
    return fresh and hmac.compare_digest("sha256=" + expected, signature)
```

Answer with any 2xx status. Network errors, 5xx, 408 and 429 are
retried in the background, waiting longer each time, for about five
hours; other statuses are given up at once. A webhook's page lists its
latest deliveries with what came back, and **Send a test** sends a
`ping` event. Deliveries are kept for 30 days.

Webhooks don't follow redirects, and don't go to private or local
addresses unless `webhooks.allow_private_addresses` is on in the
[configuration](../configuration.md#webhooks).
