//! The tag box's metatags (moekura_core::post_edit) that change more than
//! the post's own fields: children, pools, favorites, favorite groups and
//! votes. [`prepare`] checks each against its own permission before the
//! post is saved; [`apply`] carries them out after. Children's parents
//! and pools change with a history entry, as they would by hand.

use moekura_core::permissions::Permission;
use moekura_core::pools::PoolName;
use moekura_core::post_edit::Metatag;
use moekura_core::search::PoolRef;
use moekura_db::pools::{self, Contents, Pool};
use moekura_db::{favorite_groups, favorites, posts};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::edit::Refused;
use crate::error::AppError;

/// A metatag checked and ready to apply.
#[derive(Debug)]
pub(crate) enum Effect {
    /// Makes the post the parent of this one (or, if false, stops it
    /// being so).
    Child(i64, bool),
    AddToPool(Pool),
    RemoveFromPool(Pool),
    /// Starts a pool with this (normalised) name.
    NewPool(String),
    Favorite(bool),
    Group(i32, bool),
    Vote(i16),
}

fn refused(message: impl Into<String>) -> Refused {
    Refused::Invalid(message.into())
}

/// Errors that tell the user what's wrong become messages for the form.
fn from_app(error: AppError) -> Refused {
    match error {
        AppError::Unprocessable(message) => Refused::Invalid(message),
        error => Refused::Error(error),
    }
}

/// Checks `current` may use each metatag, and finds the pools and groups
/// they name. `post_id` is the post edited (none yet for an upload).
pub(crate) async fn prepare(
    state: &AppState,
    current: &CurrentUser,
    post_id: Option<i64>,
    metatags: &[Metatag],
) -> Result<Vec<Effect>, Refused> {
    let db = state.db.primary();
    let signed_in = |what: &str| {
        current
            .user
            .as_ref()
            .map(|u| u.id)
            .ok_or_else(|| refused(format!("Log in to {what}.")))
    };
    let may = |permission: Permission, what: &str| {
        if current.can(permission) {
            Ok(())
        } else {
            Err(refused(format!("You can't {what}.")))
        }
    };
    let mut effects = Vec::new();
    for metatag in metatags {
        let effect = match metatag {
            Metatag::Rating(_)
            | Metatag::Parent(_)
            | Metatag::RemoveParent(_)
            | Metatag::Source(_) => continue,
            Metatag::Child(child) | Metatag::RemoveChild(child) => {
                may(Permission::EditPosts, "change other posts")?;
                if Some(*child) == post_id {
                    return Err(refused("A post can't be its own child."));
                }
                let post = posts::by_id(db, *child).await?;
                if !post.is_some_and(|p| crate::posts::visibility(current).allows(&p)) {
                    return Err(refused(format!("There is no post #{child}.")));
                }
                Effect::Child(*child, matches!(metatag, Metatag::Child(_)))
            }
            Metatag::Pool(pool) | Metatag::RemovePool(pool) => {
                may(Permission::EditPools, "change pools")?;
                signed_in("change pools")?;
                let found = crate::pools::find_typed(db, current, &pool.to_string())
                    .await
                    .map_err(from_app)?;
                if matches!(metatag, Metatag::Pool(_)) {
                    Effect::AddToPool(found)
                } else {
                    Effect::RemoveFromPool(found)
                }
            }
            Metatag::NewPool(name) => {
                may(Permission::EditPools, "change pools")?;
                signed_in("start pools")?;
                let name = PoolName::parse(name)
                    .map_err(|e| refused(format!("The pool name “{name}” {e}.")))?;
                match pools::by_name(db, name.as_str()).await? {
                    // As Danbooru does: the pool of that name, if any.
                    Some(existing) if !existing.is_deleted => Effect::AddToPool(existing),
                    Some(_) => {
                        return Err(refused(format!(
                            "The pool “{}” is deleted.",
                            PoolName::display(name.as_str())
                        )));
                    }
                    None => Effect::NewPool(name.as_str().to_owned()),
                }
            }
            Metatag::Favorite(on) => {
                may(Permission::Favorite, "favorite posts")?;
                signed_in("favorite posts")?;
                Effect::Favorite(*on)
            }
            Metatag::FavoriteGroup(group, on) => {
                may(Permission::Favorite, "change favorite groups")?;
                let user = signed_in("change favorite groups")?;
                let found = match group {
                    PoolRef::Id(id) => favorite_groups::by_id(db, *id).await?,
                    PoolRef::Name(name) => favorite_groups::by_name(db, user, name).await?,
                };
                let found = found
                    .filter(|g| g.creator_id == user)
                    .ok_or_else(|| refused(format!("You have no favorite group “{group}”.")))?;
                Effect::Group(found.id, *on)
            }
            Metatag::Vote(score) => {
                may(Permission::Vote, "vote on posts")?;
                let user = signed_in("vote on posts")?;
                // Not on their own post, which an upload is.
                let own = match post_id {
                    Some(id) => posts::by_id(db, id)
                        .await?
                        .is_some_and(|p| p.uploader_id == Some(user)),
                    None => true,
                };
                if own && *score != 0 {
                    return Err(refused("You can't vote on your own post."));
                }
                Effect::Vote(*score)
            }
        };
        effects.push(effect);
    }
    Ok(effects)
}

/// Applies [`prepare`]d metatags to post `post_id`, in order.
pub(crate) async fn apply(
    state: &AppState,
    current: &CurrentUser,
    post_id: i64,
    effects: &[Effect],
) -> Result<(), Refused> {
    let db = state.db.primary();
    let user = current.user.as_ref().map(|u| u.id);
    for effect in effects {
        match effect {
            Effect::Child(child, true) => {
                crate::edit::set_parent(state, current, *child, Some(post_id)).await?;
            }
            Effect::Child(child, false) => {
                let is_child = posts::by_id(db, *child)
                    .await?
                    .is_some_and(|p| p.parent_id == Some(post_id));
                if is_child {
                    crate::edit::set_parent(state, current, *child, None).await?;
                }
            }
            Effect::AddToPool(pool) => {
                let has = pools::post_ids(db, pool.id).await?.contains(&post_id);
                if !has {
                    crate::pools::append(state, current, pool, post_id)
                        .await
                        .map_err(from_app)?;
                }
            }
            Effect::RemoveFromPool(pool) => {
                let Some(mut contents) = pools::contents(db, pool.id).await? else {
                    continue;
                };
                if contents.post_ids.contains(&post_id) {
                    contents.post_ids.retain(|&id| id != post_id);
                    pools::save(db, pool.id, &contents, user, None)
                        .await
                        .map_err(|e| from_app(crate::pools::save_error(e)))?;
                }
            }
            Effect::NewPool(name) => {
                let contents = Contents {
                    name: name.clone(),
                    description: String::new(),
                    category: "series".into(),
                    is_deleted: false,
                    post_ids: vec![post_id],
                };
                let id = pools::create(db, &contents, user)
                    .await
                    .map_err(|e| from_app(crate::pools::save_error(e)))?;
                tracing::info!(pool = id, name, "pool created from a metatag");
            }
            Effect::Favorite(on) => {
                let Some(user) = user else { continue };
                if *on {
                    favorites::add(db, user, post_id).await?;
                } else {
                    favorites::remove(db, user, post_id).await?;
                }
            }
            Effect::Group(group, true) => {
                crate::favorite_groups::append(state, current, *group, post_id)
                    .await
                    .map_err(from_app)?;
            }
            Effect::Group(group, false) => {
                favorite_groups::remove_post(db, *group, post_id).await?;
            }
            Effect::Vote(score) => {
                let Some(user) = user else { continue };
                crate::favorites::vote_on(db, post_id, user, *score)
                    .await
                    .map_err(from_app)?;
            }
        }
    }
    Ok(())
}
