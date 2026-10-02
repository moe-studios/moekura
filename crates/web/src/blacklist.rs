//! Applying the viewer's blacklist: their own, or the site's default.
//!
//! Searches leave blacklisted posts out in SQL ([`exclusions`]), so their
//! pages stay full and their counts right. Grids of given posts (pools,
//! favorite groups, comments, …) and post pages check each post with
//! [`Active::matching`] instead, as do searches of viewers who chose to
//! see blacklisted posts blurred.

use std::collections::HashMap;

use moekura_core::blacklist::Blacklist;
use moekura_core::posts::Rating;
use moekura_core::user_settings::UserSettings;
use moekura_db::search::Exclusion;
use moekura_db::{tag_relations, tags};
use sqlx::PgPool;

use crate::AppState;
use crate::auth::CurrentUser;

/// A blacklist with its tags resolved to ids.
pub struct Active {
    list: Blacklist,
    /// Tag name as written (after following aliases) → tag id.
    ids: HashMap<String, i32>,
}

/// The blacklist text that applies to `current`.
pub fn text_for(state: &AppState, current: &CurrentUser) -> String {
    current
        .user
        .as_ref()
        .and_then(|u| UserSettings::from_json(&u.settings).blacklist)
        .unwrap_or_else(|| state.site.get().settings.default_blacklist.clone())
}

/// Whether `current` sees blacklisted posts blurred rather than left out.
pub fn blurs(current: &CurrentUser) -> bool {
    current
        .user
        .as_ref()
        .is_some_and(|u| UserSettings::from_json(&u.settings).blur_blacklisted)
}

/// What searches leave out for `current`: their blacklist, unless they
/// see blacklisted posts blurred instead.
pub async fn exclusions(
    state: &AppState,
    db: &PgPool,
    current: &CurrentUser,
) -> sqlx::Result<Vec<Exclusion>> {
    if blurs(current) {
        return Ok(Vec::new());
    }
    Ok(for_viewer(state, db, current)
        .await?
        .map(|list| list.exclusions())
        .unwrap_or_default())
}

/// The viewer's blacklist, or `None` when it's empty. A stored blacklist
/// that no longer parses (the rules got stricter) is ignored.
pub async fn for_viewer(
    state: &AppState,
    db: &PgPool,
    current: &CurrentUser,
) -> sqlx::Result<Option<Active>> {
    let Ok(list) = Blacklist::parse(&text_for(state, current)) else {
        return Ok(None);
    };
    if list.is_empty() {
        return Ok(None);
    }
    let names: Vec<&str> = list.tag_names().map(|n| n.as_str()).collect();
    // `kitty` in a blacklist also hides posts tagged with its alias `cat`.
    let aliases: HashMap<String, String> = tag_relations::aliases_of(db, &names)
        .await?
        .into_iter()
        .collect();
    let targets: Vec<&str> = names
        .iter()
        .map(|n| aliases.get(*n).map_or(*n, String::as_str))
        .collect();
    let found: HashMap<String, i32> = tags::by_names(db, &targets)
        .await?
        .into_iter()
        .map(|t| (t.name, t.id))
        .collect();
    let ids = names
        .iter()
        .filter_map(|name| {
            let target = aliases.get(*name).map_or(*name, String::as_str);
            found.get(target).map(|id| ((*name).to_owned(), *id))
        })
        .collect();
    Ok(Some(Active { list, ids }))
}

impl Active {
    /// The rule a post with these tags and rating matches, if any.
    pub fn matching(&self, rating: Rating, tag_ids: &[i32]) -> Option<&str> {
        self.list
            .matching(rating, |name| {
                self.ids
                    .get(name.as_str())
                    .is_some_and(|id| tag_ids.contains(id))
            })
            .map(|rule| rule.text.as_str())
    }

    /// The rules as exclusions for a search. Rules needing a tag that
    /// doesn't exist match nothing, so they're left out.
    pub fn exclusions(&self) -> Vec<Exclusion> {
        let id = |name: &moekura_core::tags::TagName| self.ids.get(name.as_str()).copied();
        self.list
            .rules
            .iter()
            .filter_map(|rule| {
                Some(Exclusion {
                    tags: rule.include.iter().map(id).collect::<Option<_>>()?,
                    not_tags: rule.exclude.iter().filter_map(id).collect(),
                    ratings: rule.ratings.clone(),
                    not_ratings: rule.not_ratings.clone(),
                })
            })
            .collect()
    }
}
