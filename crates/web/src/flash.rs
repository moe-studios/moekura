//! One-shot messages shown on the page after a redirect ("Logged in").
//!
//! The cookie carries only a fixed key, never user-supplied text, so it
//! can't be used to inject content into a page.

use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};

pub const COOKIE: &str = "moekura_flash";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flash {
    LoggedIn,
    LoggedOut,
    Registered,
    AwaitingApproval,
    Saved,
    ApiKeyRevoked,
    /// A link was emailed to confirm an address.
    CheckEmail,
    EmailConfirmed,
    PasswordChanged,
    /// Done, but some of the items were dealt with meanwhile.
    SomeSkipped,
    /// Left to a background job.
    Queued,
    /// Held by the spam filter for the staff to check.
    Held,
}

impl Flash {
    const ALL: [Flash; 12] = [
        Flash::LoggedIn,
        Flash::LoggedOut,
        Flash::Registered,
        Flash::AwaitingApproval,
        Flash::Saved,
        Flash::ApiKeyRevoked,
        Flash::CheckEmail,
        Flash::EmailConfirmed,
        Flash::PasswordChanged,
        Flash::SomeSkipped,
        Flash::Queued,
        Flash::Held,
    ];

    /// As in the cookie; the message is `flash-<key>`.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Flash::LoggedIn => "logged_in",
            Flash::LoggedOut => "logged_out",
            Flash::Registered => "registered",
            Flash::AwaitingApproval => "awaiting_approval",
            Flash::Saved => "saved",
            Flash::ApiKeyRevoked => "api_key_revoked",
            Flash::CheckEmail => "check_email",
            Flash::EmailConfirmed => "email_confirmed",
            Flash::PasswordChanged => "password_changed",
            Flash::SomeSkipped => "some_skipped",
            Flash::Queued => "queued",
            Flash::Held => "held",
        }
    }

    pub fn from_jar(jar: &CookieJar) -> Option<Self> {
        let value = jar.get(COOKIE)?.value().to_owned();
        Self::ALL.into_iter().find(|f| f.key() == value)
    }
}

/// Queues `flash` for the next page the browser loads.
pub fn set(jar: CookieJar, flash: Flash) -> CookieJar {
    jar.add(
        Cookie::build((COOKIE, flash.key()))
            .path("/")
            .http_only(true)
            .same_site(SameSite::Lax)
            .max_age(time::Duration::minutes(5))
            .build(),
    )
}

pub fn removal() -> Cookie<'static> {
    Cookie::build((COOKIE, ""))
        .path("/")
        .max_age(time::Duration::ZERO)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_cookie() {
        for flash in Flash::ALL {
            let jar = set(CookieJar::new(), flash);
            assert_eq!(Flash::from_jar(&jar), Some(flash));
        }
        let forged = CookieJar::new().add(Cookie::new(COOKIE, "<b>hi</b>"));
        assert_eq!(Flash::from_jar(&forged), None);
    }
}
