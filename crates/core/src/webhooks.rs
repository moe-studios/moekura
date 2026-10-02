//! Outgoing webhooks: the events a site sends, how deliveries are
//! signed, and the formats they're written in.

pub mod discord;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// Something that happened on the site, which webhooks subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    PostCreated,
    PostApproved,
    PostDeleted,
    PostFlagged,
    CommentCreated,
    UserRegistered,
    /// Sent by "send test" only.
    Ping,
}

impl Event {
    /// The events a webhook can subscribe to.
    pub const SUBSCRIBABLE: [Event; 6] = [
        Event::PostCreated,
        Event::PostApproved,
        Event::PostDeleted,
        Event::PostFlagged,
        Event::CommentCreated,
        Event::UserRegistered,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Event::PostCreated => "post.created",
            Event::PostApproved => "post.approved",
            Event::PostDeleted => "post.deleted",
            Event::PostFlagged => "post.flagged",
            Event::CommentCreated => "comment.created",
            Event::UserRegistered => "user.registered",
            Event::Ping => "ping",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Event::PostCreated => "A post is uploaded",
            Event::PostApproved => "A post is approved",
            Event::PostDeleted => "A post is deleted",
            Event::PostFlagged => "A post is flagged",
            Event::CommentCreated => "A comment is posted",
            Event::UserRegistered => "Someone registers",
            Event::Ping => "A test",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::SUBSCRIBABLE
            .into_iter()
            .chain([Event::Ping])
            .find(|e| e.as_str() == s)
    }
}

/// How deliveries to a webhook are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// Moekura's own signed JSON.
    Moekura,
    /// Discord's execute-webhook payload: a message with one embed.
    Discord,
}

impl Format {
    pub const ALL: [Format; 2] = [Format::Moekura, Format::Discord];

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Moekura => "moekura",
            Format::Discord => "discord",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == s)
    }

    /// The format a webhook URL wants: Discord's for Discord webhook URLs,
    /// otherwise Moekura's.
    pub fn detect(url: &str) -> Self {
        match url::Url::parse(url) {
            Ok(url) if discord::is_webhook_url(&url) => Format::Discord,
            _ => Format::Moekura,
        }
    }
}

/// The signature of a delivery: HMAC-SHA256 of `{timestamp}.{body}` with
/// the webhook's secret, as lowercase hex. Receivers compute the same and
/// compare, and reject old timestamps to stop replays.
pub fn signature(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes keys of any length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_round_trip() {
        for event in Event::SUBSCRIBABLE {
            assert_eq!(Event::parse(event.as_str()), Some(event));
        }
        assert_eq!(Event::parse("ping"), Some(Event::Ping));
        assert_eq!(Event::parse("nope"), None);
    }

    #[test]
    fn detects_discord_urls() {
        for url in [
            "https://discord.com/api/webhooks/123/abc-DEF_1",
            "https://discordapp.com/api/webhooks/123/abc",
            "https://ptb.discord.com/api/webhooks/123/abc?thread_id=4",
            "https://canary.discord.com/api/v10/webhooks/123/abc",
        ] {
            assert_eq!(Format::detect(url), Format::Discord, "{url}");
        }
        for url in [
            "https://hooks.example/api/webhooks/123/abc",
            "http://discord.com/api/webhooks/123/abc",
            "https://discord.com/api/webhooks/123",
            "https://discord.com/api/webhooks/abc/def",
            "https://evil.discord.com.example/api/webhooks/123/abc",
            "not a url",
        ] {
            assert_eq!(Format::detect(url), Format::Moekura, "{url}");
        }
        for format in Format::ALL {
            assert_eq!(Format::parse(format.as_str()), Some(format));
        }
    }

    #[test]
    fn signs_like_a_receiver_would() {
        // printf '1700000000.{"a":1}' | openssl dgst -sha256 -hmac secret
        assert_eq!(
            signature("secret", 1_700_000_000, br#"{"a":1}"#),
            "49f24e537407743fa4a0242bb63b94b9a47ee99cbbe071ccd8a22550ae411686"
        );
    }
}
