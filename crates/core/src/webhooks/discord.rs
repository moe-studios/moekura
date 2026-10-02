//! Deliveries to Discord webhooks: an execute-webhook payload with one
//! embed per event, cut to Discord's limits, that can't ping anyone.

use std::str::FromStr;

use serde_json::{Map, Value, json};
use url::Url;

use super::Event;
use crate::posts::Rating;

/// Discord's limits, in characters.
const TITLE_LEN: usize = 256;
const DESCRIPTION_LEN: usize = 4096;
const FIELD_NAME_LEN: usize = 256;
const FIELD_VALUE_LEN: usize = 1024;
const AUTHOR_LEN: usize = 256;
const CONTENT_LEN: usize = 2000;
pub const USERNAME_LEN: usize = 80;

/// Whether `url` is a Discord webhook:
/// `https://discord.com/api/webhooks/{id}/{token}` (also on
/// `discordapp.com`, `ptb.` and `canary.`, and with an API version).
pub fn is_webhook_url(url: &Url) -> bool {
    let host_ok = url.host_str().is_some_and(|host| {
        let base = host
            .strip_prefix("ptb.")
            .or_else(|| host.strip_prefix("canary."))
            .unwrap_or(host);
        matches!(base, "discord.com" | "discordapp.com")
    });
    if url.scheme() != "https" || !host_ok {
        return false;
    }
    let Some(segments) = url.path_segments() else {
        return false;
    };
    let mut segments: Vec<&str> = segments.filter(|s| !s.is_empty()).collect();
    if segments.get(1).is_some_and(|s| {
        s.strip_prefix('v')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    }) {
        segments.remove(1);
    }
    matches!(
        segments.as_slice(),
        ["api", "webhooks", id, token]
            if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && !token.is_empty()
    )
}

/// `url` with `wait=true`, so Discord answers with the message it made.
pub fn wait_url(url: &Url) -> Url {
    let mut url = url.clone();
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| k != "wait")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(kept)
        .append_pair("wait", "true");
    url
}

/// What a webhook says beyond the event itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options<'a> {
    /// Overrides the webhook's name in Discord, when not empty.
    pub username: &'a str,
    /// Overrides the webhook's avatar, when not empty.
    pub avatar_url: &'a str,
    /// Rating codes whose posts may show their image.
    pub image_ratings: &'a [String],
}

/// The message for `event` with moekura's `data` for it, sent at
/// `timestamp` (RFC 3339).
pub fn message(event: &str, data: &Value, timestamp: &str, options: &Options) -> Value {
    let mut message = Map::new();
    match Event::parse(event) {
        Some(Event::Ping) => {
            let site = str_of(data, "site_name")
                .or_else(|| str_of(data, "site"))
                .unwrap_or("moekura");
            message.insert(
                "content".into(),
                json!(cut(
                    &format!(
                        "Moekura connected: events from {} will show up here.",
                        escape(site)
                    ),
                    CONTENT_LEN
                )),
            );
        }
        Some(event) => {
            let mut embed = match event {
                Event::CommentCreated => comment_embed(data),
                Event::UserRegistered => user_embed(data),
                _ => post_embed(event, data, options),
            };
            embed.insert("color".into(), json!(colour(event)));
            if !timestamp.is_empty() {
                embed.insert("timestamp".into(), json!(timestamp));
            }
            message.insert("embeds".into(), json!([embed]));
        }
        None => {
            message.insert(
                "content".into(),
                json!(cut(
                    &format!("Moekura event `{}`", event.replace('`', "'")),
                    CONTENT_LEN
                )),
            );
        }
    }
    if !options.username.is_empty() {
        message.insert(
            "username".into(),
            json!(cut(options.username, USERNAME_LEN)),
        );
    }
    if !options.avatar_url.is_empty() {
        message.insert("avatar_url".into(), json!(options.avatar_url));
    }
    // Nobody gets pinged, whatever a comment or tag says.
    message.insert("allowed_mentions".into(), json!({ "parse": [] }));
    Value::Object(message)
}

fn colour(event: Event) -> u32 {
    match event {
        Event::PostCreated => 0x0075f8,
        Event::PostApproved => 0x2ecc71,
        Event::PostDeleted => 0xed4245,
        Event::PostFlagged => 0xf1a40f,
        Event::CommentCreated => 0x9b59b6,
        Event::UserRegistered => 0x1abc9c,
        Event::Ping => 0x99aab5,
    }
}

fn post_embed(event: Event, data: &Value, options: &Options) -> Map<String, Value> {
    let id = data
        .get("post_id")
        .map(Value::to_string)
        .unwrap_or_default();
    let verb = match event {
        Event::PostApproved => "approved",
        Event::PostDeleted => "deleted",
        Event::PostFlagged => "flagged",
        _ => "uploaded",
    };
    let mut embed = base_embed(&format!("Post #{id} {verb}"), data);
    author(&mut embed, data, "uploader", "uploader_url");

    let mut fields = Vec::new();
    let rating = str_of(data, "rating").unwrap_or_default();
    if let Ok(parsed) = Rating::from_str(rating) {
        fields.push(field("Rating", parsed.label(), true));
    }
    if let Some(status) = str_of(data, "status").filter(|s| *s != "active") {
        fields.push(field("Status", status, true));
    }
    let tags: Vec<&str> = data
        .get("tags")
        .and_then(Value::as_array)
        .map(|tags| tags.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !tags.is_empty() {
        fields.push(field(
            &format!("Tags ({})", tags.len()),
            &tag_list(&tags, FIELD_VALUE_LEN),
            false,
        ));
    }
    if let Some(source) = str_of(data, "source").filter(|s| !s.is_empty()) {
        fields.push(field("Source", source, false));
    }
    if let Some(reason) = str_of(data, "reason").filter(|s| !s.is_empty()) {
        fields.push(field("Reason", &escape(reason), false));
    }
    embed.insert("fields".into(), json!(fields));

    let shows = options.image_ratings.iter().any(|r| r == rating);
    if let Some(image) = str_of(data, "image_url").filter(|_| shows) {
        embed.insert("image".into(), json!({ "url": image }));
    }
    embed
}

fn comment_embed(data: &Value) -> Map<String, Value> {
    let post = data
        .get("post_id")
        .map(Value::to_string)
        .unwrap_or_default();
    let mut embed = base_embed(&format!("Comment on post #{post}"), data);
    author(&mut embed, data, "author", "author_url");
    if let Some(body) = str_of(data, "body") {
        embed.insert("description".into(), json!(cut(body, DESCRIPTION_LEN)));
    }
    embed
}

fn user_embed(data: &Value) -> Map<String, Value> {
    let name = str_of(data, "name").unwrap_or_default();
    let mut embed = base_embed(&format!("{} registered", escape(name)), data);
    if let Some(status) = str_of(data, "status").filter(|s| *s != "active") {
        embed.insert("fields".into(), json!([field("Status", status, true)]));
    }
    embed
}

/// An embed with `title`, linking to `data`'s URL.
fn base_embed(title: &str, data: &Value) -> Map<String, Value> {
    let mut embed = Map::new();
    embed.insert("title".into(), json!(cut(title, TITLE_LEN)));
    if let Some(url) = str_of(data, "url") {
        embed.insert("url".into(), json!(url));
    }
    embed
}

/// Credits the user named at `name`, linked to `url`, when there is one.
fn author(embed: &mut Map<String, Value>, data: &Value, name: &str, url: &str) {
    if let Some(name) = str_of(data, name).filter(|n| !n.is_empty()) {
        let mut author = json!({ "name": cut(name, AUTHOR_LEN) });
        if let Some(url) = str_of(data, url) {
            author["url"] = json!(url);
        }
        embed.insert("author".into(), author);
    }
}

fn field(name: &str, value: &str, inline: bool) -> Value {
    json!({
        "name": cut(name, FIELD_NAME_LEN),
        "value": cut(value, FIELD_VALUE_LEN),
        "inline": inline,
    })
}

/// As many of `tags` as fit in `max` characters, then how many more.
fn tag_list(tags: &[&str], max: usize) -> String {
    let mut list = String::new();
    for (i, tag) in tags.iter().enumerate() {
        let tag = escape(tag);
        let more = tags.len() - i;
        let tail = if more > 1 {
            format!(" and {} more", more - 1)
        } else {
            String::new()
        };
        let needed = usize::from(!list.is_empty()) + tag.chars().count();
        // Room for this tag and, if others follow, for saying so.
        if list.chars().count() + needed + tail.chars().count() > max {
            if list.is_empty() {
                return cut(&format!("{tag}{tail}"), max);
            }
            list.push_str(&format!(" and {more} more"));
            return list;
        }
        if !list.is_empty() {
            list.push(' ');
        }
        list.push_str(&tag);
    }
    list
}

fn str_of<'a>(data: &'a Value, key: &str) -> Option<&'a str> {
    data.get(key).and_then(Value::as_str)
}

/// `text` shortened to `max` characters, ending in `…` when cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max - 1).collect();
    // Don't leave half an escape behind.
    if cut.ends_with('\\') {
        cut.pop();
    }
    cut.push('…');
    cut
}

/// `text` with Discord's markdown characters escaped, so tag names like
/// `*_*` show as written.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '~' | '`' | '|' | '>' | '#' | '[' | ']' | '(' | ')' | '<' | '@'
        ) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: &str = "2026-10-02T12:00:00Z";

    fn post(rating: &str) -> Value {
        json!({
            "post_id": 12,
            "url": "https://booru.example/posts/12",
            "status": "active",
            "rating": rating,
            "source": "https://x.com/a/status/1",
            "tags": ["cat", "*_*", "solo"],
            "uploader": "alice",
            "uploader_url": "https://booru.example/users/alice",
            "image_url": "https://booru.example/data/sample/ab/cd.jpg",
            "created_at": AT,
        })
    }

    fn safe() -> Vec<String> {
        vec!["g".into(), "s".into()]
    }

    #[test]
    fn posts_become_embeds() {
        let ratings = safe();
        let options = Options {
            image_ratings: &ratings,
            ..Options::default()
        };
        assert_eq!(
            message("post.created", &post("g"), AT, &options),
            json!({
                "embeds": [{
                    "title": "Post #12 uploaded",
                    "url": "https://booru.example/posts/12",
                    "author": { "name": "alice", "url": "https://booru.example/users/alice" },
                    "fields": [
                        { "name": "Rating", "value": "General", "inline": true },
                        { "name": "Tags (3)", "value": "cat \\*\\_\\* solo", "inline": false },
                        { "name": "Source", "value": "https://x.com/a/status/1", "inline": false },
                    ],
                    "image": { "url": "https://booru.example/data/sample/ab/cd.jpg" },
                    "color": 0x0075f8,
                    "timestamp": AT,
                }],
                "allowed_mentions": { "parse": [] },
            })
        );

        let mut deleted = post("g");
        deleted["status"] = json!("deleted");
        deleted["reason"] = json!("@everyone dupe");
        let sent = message("post.deleted", &deleted, AT, &options);
        let embed = &sent["embeds"][0];
        assert_eq!(embed["title"], "Post #12 deleted");
        assert_eq!(embed["color"], 0xed4245);
        assert_eq!(
            embed["fields"][1],
            json!({ "name": "Status", "value": "deleted", "inline": true })
        );
        assert_eq!(embed["fields"][4]["value"], "\\@everyone dupe");
    }

    #[test]
    fn images_only_for_chosen_ratings() {
        let ratings = safe();
        let options = Options {
            image_ratings: &ratings,
            ..Options::default()
        };
        for (rating, shown) in [("g", true), ("s", true), ("q", false), ("e", false)] {
            let sent = message("post.approved", &post(rating), AT, &options);
            assert_eq!(sent["embeds"][0].get("image").is_some(), shown, "{rating}");
        }
        // Nor without an image to show.
        let mut bare = post("g");
        bare.as_object_mut().unwrap().remove("image_url");
        let sent = message("post.created", &bare, AT, &options);
        assert!(sent["embeds"][0].get("image").is_none());
    }

    #[test]
    fn comments_are_cut_and_ping_nobody() {
        let body = format!("@everyone <@&123> {}", "a".repeat(5000));
        let data = json!({
            "comment_id": 3,
            "post_id": 12,
            "url": "https://booru.example/posts/12#comment-3",
            "author": "bob",
            "author_url": "https://booru.example/users/bob",
            "body": body,
        });
        let options = Options {
            username: "Booru",
            avatar_url: "https://booru.example/icon.png",
            image_ratings: &[],
        };
        let sent = message("comment.created", &data, AT, &options);
        let embed = &sent["embeds"][0];
        assert_eq!(embed["title"], "Comment on post #12");
        assert_eq!(embed["author"]["name"], "bob");
        let description = embed["description"].as_str().unwrap();
        assert_eq!(description.chars().count(), DESCRIPTION_LEN);
        assert!(description.starts_with("@everyone") && description.ends_with('…'));
        assert_eq!(sent["allowed_mentions"], json!({ "parse": [] }));
        assert_eq!(sent["username"], "Booru");
        assert_eq!(sent["avatar_url"], "https://booru.example/icon.png");
    }

    #[test]
    fn users_and_tests() {
        let data = json!({
            "user_id": 4,
            "name": "new_user",
            "url": "https://booru.example/users/new_user",
            "status": "active",
        });
        let sent = message("user.registered", &data, AT, &Options::default());
        assert_eq!(sent["embeds"][0]["title"], "new\\_user registered");
        assert_eq!(
            sent["embeds"][0]["url"],
            "https://booru.example/users/new_user"
        );
        assert!(sent["embeds"][0].get("fields").is_none());

        let ping = message(
            "ping",
            &json!({ "site_name": "Booru", "site": "https://booru.example/" }),
            AT,
            &Options::default(),
        );
        assert_eq!(
            ping,
            json!({
                "content": "Moekura connected: events from Booru will show up here.",
                "allowed_mentions": { "parse": [] },
            })
        );
    }

    #[test]
    fn long_tag_lists_say_how_many_more() {
        let tags: Vec<String> = (0..300).map(|i| format!("tag_number_{i}")).collect();
        let tags: Vec<&str> = tags.iter().map(String::as_str).collect();
        let list = tag_list(&tags, FIELD_VALUE_LEN);
        assert!(list.chars().count() <= FIELD_VALUE_LEN, "{}", list.len());
        assert!(list.ends_with(" more"), "{list}");
        assert_eq!(tag_list(&["a", "b"], 100), "a b");
        assert_eq!(tag_list(&["abcdef"], 4), "abc…");
    }

    #[test]
    fn waits_for_the_message() {
        let url =
            Url::parse("https://discord.com/api/webhooks/1/t?thread_id=9&wait=false").unwrap();
        assert_eq!(
            wait_url(&url).as_str(),
            "https://discord.com/api/webhooks/1/t?thread_id=9&wait=true"
        );
    }
}
