//! Applying the viewer's blacklist: their own, or the site's default.

use std::collections::HashMap;

use sqlx::PgPool;
use uwu_core::blacklist::Blacklist;
use uwu_core::posts::Rating;
use uwu_core::user_settings::UserSettings;
use uwu_db::{tag_relations, tags};

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
}
