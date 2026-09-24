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
}

impl Flash {
    const ALL: [Flash; 6] = [
        Flash::LoggedIn,
        Flash::LoggedOut,
        Flash::Registered,
        Flash::AwaitingApproval,
        Flash::Saved,
        Flash::ApiKeyRevoked,
    ];

    fn key(self) -> &'static str {
        match self {
            Flash::LoggedIn => "logged_in",
            Flash::LoggedOut => "logged_out",
            Flash::Registered => "registered",
            Flash::AwaitingApproval => "awaiting_approval",
            Flash::Saved => "saved",
            Flash::ApiKeyRevoked => "api_key_revoked",
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Flash::LoggedIn => "Welcome back!",
            Flash::LoggedOut => "You have been logged out.",
            Flash::Registered => "Your account is ready. Welcome!",
            Flash::AwaitingApproval => {
                "Your account was created and is waiting for approval by the staff."
            }
            Flash::Saved => "Saved.",
            Flash::ApiKeyRevoked => "The API key was revoked.",
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
