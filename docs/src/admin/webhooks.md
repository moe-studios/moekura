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

Each webhook has a format: **Moekura JSON**, signed (below), or
**Discord**, for a [Discord webhook](#discord). The form picks Discord
by itself for Discord webhook URLs.

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

Post events carry `post_id`, `url`, `status`, `rating`, `source`,
`tags`, `uploader` and `uploader_url`, `created_at`, and `image_url`: a
sample of the image (or a video's poster), only for ratings visitors who
aren't logged in may see and never for deleted posts; deletions and
flags add a `reason`. Comments carry `comment_id`, `post_id`, `url`,
`author`, `author_url`, `body` and `created_at`; registrations
`user_id`, `name`, `url` and `status`.

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
hours (or, for a 429 with `Retry-After`, as long as that says, up to an
hour); other statuses are given up at once. A webhook's page lists its
latest deliveries with what came back, and **Send a test** sends a
`ping` event. Deliveries are kept for 30 days.

Webhooks don't follow redirects, and don't go to private or local
addresses unless `webhooks.allow_private_addresses` is on in the
[configuration](../configuration.md#webhooks).

## Discord

To post events in a Discord channel, make a webhook in the channel's
settings (**Integrations → Webhooks → New Webhook**), copy its URL
(`https://discord.com/api/webhooks/…/…`) and add it here. Each event
becomes a message with one embed: a link to the post, comment or user,
who made it, the rating, tags (as many as fit), source and reason, and
for posts the image. **Send a test** posts a short "Moekura connected"
message.

- Images show only for posts whose rating is ticked under **Images in
  Discord messages** (general and sensitive at first) *and* that
  visitors who aren't logged in may see (the `visitor_ratings` site
  setting). Others get a message without the image. Private sites
  never send images, since Discord couldn't load them.
- Nobody is pinged: messages turn off `@everyone`, role and user
  mentions, whatever a comment says.
- **Name** and **Avatar URL** replace the webhook's own name and avatar
  in the messages. Discord doesn't allow names containing "discord" or
  "clyde".
- There are no signature headers and no secret: the token in the URL
  is the secret, so keep the URL private.
- Rate limits are honoured: on 429 the delivery waits as long as
  Discord asks before trying again.
- Deliveries are sent with `?wait=true`, so the log shows the id of
  the message Discord made.

Discord is on the public internet, so `allow_private_addresses` isn't
needed. Messages to a thread can use the URL's `thread_id` parameter as
usual.
