//! Moderation: reviewing posts and flags, deciding tag relations, bans
//! and the moderation log.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_db::mod_actions::{self, Entry, Filter};
use moekura_db::post_batches::{self, PostBatch};
use moekura_db::{bans, flags, users};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};

use super::posts::{ApiPost, one};
use super::tags::ApiRelation;
use crate::AppState;
use crate::auth::{CurrentUser, RequestInfo};
use crate::error::{AppError, ErrorBody};
use crate::moderation::PostAction;
use crate::post_batches::PurgeScope;
use crate::tag_relations::Decision;

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct Reason {
    /// Shown on the post and in the moderation log.
    #[serde(default)]
    reason: String,
}

async fn act(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    action: PostAction,
    reason: &str,
) -> Result<Json<ApiPost>, AppError> {
    crate::moderation::moderate(state, current, id, action, reason).await?;
    Ok(Json(one(state, current, id).await?))
}

/// Approve a pending post.
///
/// Needs `approve_posts`.
#[utoipa::path(
    post,
    path = "/posts/{id}/approve",
    operation_id = "approve_post",
    tag = "moderation",
    params(("id" = i64, Path, description = "Post number")),
    responses(
        (status = 200, body = ApiPost),
        (status = 400, body = ErrorBody, description = "The post isn't pending"),
    ),
)]
pub(crate) async fn approve(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<ApiPost>, AppError> {
    act(&state, &current, id, PostAction::Approve, "").await
}

/// Reject a pending post.
///
/// Needs `approve_posts`. The post is deleted.
#[utoipa::path(
    post,
    path = "/posts/{id}/reject",
    operation_id = "reject_post",
    tag = "moderation",
    params(("id" = i64, Path, description = "Post number")),
    request_body = Reason,
    responses(
        (status = 200, body = ApiPost),
        (status = 400, body = ErrorBody, description = "The post isn't pending"),
    ),
)]
pub(crate) async fn reject(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Json(body): Json<Reason>,
) -> Result<Json<ApiPost>, AppError> {
    act(&state, &current, id, PostAction::Reject, &body.reason).await
}

/// Appeal a deleted post.
///
/// Needs `flag`, and either the post is your upload or you can see
/// deleted posts. Asks staff to restore it; a post has one open appeal at
/// a time, and appeals are rate limited.
#[utoipa::path(
    post,
    path = "/posts/{id}/appeal",
    operation_id = "appeal_post",
    tag = "moderation",
    params(("id" = i64, Path, description = "Post number")),
    request_body = Reason,
    responses(
        (status = 204, description = "Appealed"),
        (status = 400, body = ErrorBody, description = "The post isn't deleted, is already appealed, or no reason was given"),
        (status = 429, body = ErrorBody, description = "Too many appeals lately"),
    ),
)]
pub(crate) async fn appeal(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Json(body): Json<Reason>,
) -> Result<StatusCode, AppError> {
    crate::moderation::appeal_post(&state, &current, id, &body.reason).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewDisapproval {
    /// `breaks_rules`, `poor_quality` or `disinterest`.
    #[schema(example = "poor_quality")]
    reason: String,
    /// A note for other approvers.
    #[serde(default)]
    message: String,
}

/// Disapprove a pending post.
///
/// Needs `approve_posts`. Passes on the post without rejecting it: it
/// leaves your approval queue (`status:unmoderated`), and other approvers
/// see why. Disapproving again replaces your earlier reason.
#[utoipa::path(
    post,
    path = "/posts/{id}/disapprove",
    operation_id = "disapprove_post",
    tag = "moderation",
    params(("id" = i64, Path, description = "Post number")),
    request_body = NewDisapproval,
    responses(
        (status = 204, description = "Disapproved"),
        (status = 400, body = ErrorBody, description = "The post isn't pending, or the reason is unknown"),
    ),
)]
pub(crate) async fn disapprove(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Json(body): Json<NewDisapproval>,
) -> Result<StatusCode, AppError> {
    crate::moderation::disapprove_post(&state, &current, id, &body.reason, &body.message).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Delete a post.
///
/// Needs `delete_posts`, and a reason, which the post shows in its place.
/// Deleted posts stay visible to moderators and can be restored; open
/// flags on the post are upheld.
#[utoipa::path(
    post,
    path = "/posts/{id}/delete",
    operation_id = "delete_post",
    tag = "moderation",
    params(("id" = i64, Path, description = "Post number")),
    request_body = Reason,
    responses(
        (status = 200, body = ApiPost),
        (status = 400, body = ErrorBody, description = "The post is already deleted, or no reason was given"),
    ),
)]
pub(crate) async fn delete(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Json(body): Json<Reason>,
) -> Result<Json<ApiPost>, AppError> {
    act(&state, &current, id, PostAction::Delete, &body.reason).await
}

/// Restore a deleted post.
///
/// Needs `delete_posts`.
#[utoipa::path(
    post,
    path = "/posts/{id}/restore",
    operation_id = "restore_post",
    tag = "moderation",
    params(("id" = i64, Path, description = "Post number")),
    responses(
        (status = 200, body = ApiPost),
        (status = 400, body = ErrorBody, description = "The post isn't deleted"),
    ),
)]
pub(crate) async fn restore(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<ApiPost>, AppError> {
    act(&state, &current, id, PostAction::Restore, "").await
}

/// Purge a deleted post.
///
/// Needs `purge_posts`. The post, its files and its history are removed
/// for good by a background job shortly after.
#[utoipa::path(
    post,
    path = "/posts/{id}/purge",
    operation_id = "purge_post",
    tag = "moderation",
    params(("id" = i64, Path, description = "Post number")),
    responses(
        (status = 202, description = "The purge is queued"),
        (status = 400, body = ErrorBody, description = "Only deleted posts can be purged"),
    ),
)]
pub(crate) async fn purge(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    crate::moderation::moderate(&state, &current, id, PostAction::Purge, "").await?;
    Ok(StatusCode::ACCEPTED)
}

/// A moderation of many posts, worked through in the background.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiPostBatch {
    pub id: i64,
    /// `delete`: deleting every upload of `user`; `purge`: purging the
    /// deleted posts `query` finds, or `post_ids`.
    #[schema(example = "delete")]
    pub kind: String,
    /// Who started it.
    pub creator: Option<String>,
    /// Whose uploads, for deletions.
    pub user: Option<String>,
    pub reason: String,
    /// The search a purge covers, with `status:deleted`.
    pub query: Option<String>,
    /// The posts a purge covers, when picked one by one.
    pub post_ids: Option<Vec<i64>>,
    /// `queued`, `running`, `done` or `failed`.
    #[schema(example = "running")]
    pub status: String,
    /// Posts it applied to when started.
    pub total: i32,
    /// Posts dealt with so far.
    pub done: i32,
    /// Posts left alone: dealt with meanwhile (restored, say), or with a
    /// locked status.
    pub skipped: i32,
    pub failed: i32,
    /// Why it failed.
    pub error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

impl From<PostBatch> for ApiPostBatch {
    fn from(b: PostBatch) -> Self {
        Self {
            id: b.id,
            kind: b.kind,
            creator: b.creator_name,
            user: b.user_name,
            reason: b.reason,
            query: b.query,
            post_ids: b.post_ids,
            status: b.status,
            total: b.total,
            done: b.done,
            skipped: b.skipped,
            failed: b.failed,
            error: b.error,
            created_at: b.created_at,
            finished_at: b.finished_at,
        }
    }
}

/// Delete every upload of a user.
///
/// Needs `delete_posts`, a role ranked above the user's, and a reason,
/// which each post shows in its place. Their active, flagged and pending
/// posts are deleted in the background, as deleting each would: open
/// flags are upheld, each deletion is logged as yours, and the posts can
/// be restored. Posts with a locked status are skipped unless you have
/// `lock_posts`. Follow the progress with `get_post_batch`.
#[utoipa::path(
    post,
    path = "/users/{name}/delete-uploads",
    operation_id = "delete_user_uploads",
    tag = "moderation",
    params(("name" = String, Path, description = "Case-insensitive")),
    request_body = Reason,
    responses(
        (status = 202, body = ApiPostBatch, description = "Started"),
        (status = 400, body = ErrorBody, description = "No reason, nothing to delete, or already being deleted"),
        (status = 403, body = ErrorBody, description = "You can't delete this user's uploads"),
    ),
)]
pub(crate) async fn delete_uploads(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(name): Path<String>,
    Json(body): Json<Reason>,
) -> Result<(StatusCode, Json<ApiPostBatch>), AppError> {
    current.require(Permission::DeletePosts)?;
    let user = users::by_name(state.db.primary(), &name)
        .await?
        .ok_or(AppError::NotFound)?;
    let batch = crate::post_batches::delete_uploads(&state, &current, &user, &body.reason).await?;
    Ok((StatusCode::ACCEPTED, Json(batch.into())))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct PurgeRequest {
    /// Purge every deleted post this search finds (`status:deleted` is
    /// implied; other statuses are refused). Empty for every deleted post.
    query: Option<String>,
    /// Or purge these posts, at most 500; those not deleted are left out.
    post_ids: Option<Vec<i64>>,
}

/// Purge deleted posts in bulk.
///
/// Needs `purge_posts`. Give either `query` or `post_ids`. The posts,
/// their files and their history are removed for good by a background
/// job, each as `purge_post` would; posts restored before it gets to them
/// are skipped. Follow the progress with `get_post_batch`.
#[utoipa::path(
    post,
    path = "/moderation/purge",
    operation_id = "purge_posts",
    tag = "moderation",
    request_body = PurgeRequest,
    responses(
        (status = 202, body = ApiPostBatch, description = "Started"),
        (status = 400, body = ErrorBody, description = "No deleted posts to purge, or both or neither of `query` and `post_ids`"),
        (status = 422, body = ErrorBody, description = "The search isn't valid"),
    ),
)]
pub(crate) async fn purge_posts(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(body): Json<PurgeRequest>,
) -> Result<(StatusCode, Json<ApiPostBatch>), AppError> {
    current.require(Permission::PurgePosts)?;
    let scope = match (body.query, body.post_ids) {
        (Some(query), None) => {
            PurgeScope::Search(Box::new(crate::post_batches::purge_search(&query)?))
        }
        (None, Some(ids)) => PurgeScope::Ticked(ids),
        _ => {
            return Err(AppError::BadRequest("Give either query or post_ids".into()));
        }
    };
    let batch = crate::post_batches::purge(&state, &current, scope).await?;
    Ok((StatusCode::ACCEPTED, Json(batch.into())))
}

/// Follow a moderation of many posts.
///
/// Needs `delete_posts` for deletions, `purge_posts` for purges.
#[utoipa::path(
    get,
    path = "/moderation/post-batches/{id}",
    operation_id = "get_post_batch",
    tag = "moderation",
    params(("id" = i64, Path)),
    responses((status = 200, body = ApiPostBatch)),
)]
pub(crate) async fn post_batch(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<ApiPostBatch>, AppError> {
    if !current.can(Permission::DeletePosts) {
        current.require(Permission::PurgePosts)?;
    }
    let batch = post_batches::by_id(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    current.require(if batch.kind == "purge" {
        Permission::PurgePosts
    } else {
        Permission::DeletePosts
    })?;
    Ok(Json(batch.into()))
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct Dismissed {
    /// How many open flags were dismissed.
    pub dismissed: u64,
}

/// Dismiss a post's flags.
///
/// Needs `approve_posts`. The post stays, and is no longer `flagged`.
#[utoipa::path(
    post,
    path = "/posts/{id}/flags/dismiss",
    operation_id = "dismiss_flags",
    tag = "moderation",
    params(("id" = i64, Path, description = "Post number")),
    responses(
        (status = 200, body = Dismissed),
        (status = 400, body = ErrorBody, description = "The post has no open flags"),
    ),
)]
pub(crate) async fn dismiss_flags(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<Dismissed>, AppError> {
    let dismissed = crate::moderation::dismiss(&state, &current, id).await?;
    Ok(Json(Dismissed { dismissed }))
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct FlaggedPost {
    pub post_id: i64,
    pub flags: Vec<ApiFlag>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiFlag {
    pub id: i64,
    pub creator: Option<String>,
    pub reason: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FlagParams {
    /// Posts to return, up to 100.
    limit: Option<i64>,
    /// For the next page: the id of the last post's first flag.
    after: Option<i64>,
}

/// List open flags.
///
/// Needs `approve_posts`. The posts whose flags have waited longest
/// first, each with its open flags, oldest first. For the next page, pass
/// the id of the last post's first flag as `after`.
#[utoipa::path(
    get,
    path = "/flags",
    operation_id = "list_flags",
    tag = "moderation",
    params(FlagParams),
    responses(
        (status = 200, body = Vec<FlaggedPost>),
        (status = 403, body = ErrorBody, description = "You can't review flags"),
    ),
)]
pub(crate) async fn open_flags(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<FlagParams>,
) -> Result<Json<Vec<FlaggedPost>>, AppError> {
    current.require(Permission::ApprovePosts)?;
    let limit = params.limit.unwrap_or(30).clamp(1, 100);
    let mut posts: Vec<FlaggedPost> = Vec::new();
    let after = params.after.unwrap_or(0);
    for flag in flags::open(state.db.primary(), after, limit).await? {
        let entry = ApiFlag {
            id: flag.id,
            creator: flag.creator_name,
            reason: flag.reason,
            created_at: flag.created_at,
        };
        match posts.last_mut() {
            Some(last) if last.post_id == flag.post_id => last.flags.push(entry),
            _ => posts.push(FlaggedPost {
                post_id: flag.post_id,
                flags: vec![entry],
            }),
        }
    }
    Ok(Json(posts))
}

async fn decide(
    state: &AppState,
    current: &CurrentUser,
    id: i32,
    decision: Decision,
) -> Result<Json<ApiRelation>, AppError> {
    crate::tag_relations::decide(state, current, id, decision).await?;
    let relation = moekura_db::tag_relations::by_id(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(relation.into()))
}

/// Approve a tag relation.
///
/// Needs `manage_tags`. It's then applied to existing posts in the
/// background.
#[utoipa::path(
    post,
    path = "/tag-relations/{id}/approve",
    operation_id = "approve_tag_relation",
    tag = "moderation",
    params(("id" = i32, Path, description = "Relation id")),
    responses(
        (status = 200, body = ApiRelation),
        (status = 422, body = ErrorBody, description = "It isn't pending, or it conflicts with active relations"),
    ),
)]
pub(crate) async fn approve_relation(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i32>,
) -> Result<Json<ApiRelation>, AppError> {
    decide(&state, &current, id, Decision::Approve).await
}

/// Reject a tag relation.
///
/// Needs `manage_tags`.
#[utoipa::path(
    post,
    path = "/tag-relations/{id}/reject",
    operation_id = "reject_tag_relation",
    tag = "moderation",
    params(("id" = i32, Path, description = "Relation id")),
    responses(
        (status = 200, body = ApiRelation),
        (status = 422, body = ErrorBody, description = "It isn't pending"),
    ),
)]
pub(crate) async fn reject_relation(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i32>,
) -> Result<Json<ApiRelation>, AppError> {
    decide(&state, &current, id, Decision::Reject).await
}

/// Remove a tag relation.
///
/// Ends an active relation (needs `manage_tags`; posts keep their tags),
/// or withdraws a pending request (its creator may too).
#[utoipa::path(
    delete,
    path = "/tag-relations/{id}",
    operation_id = "remove_tag_relation",
    tag = "moderation",
    params(("id" = i32, Path, description = "Relation id")),
    responses((status = 200, body = ApiRelation), (status = 403, body = ErrorBody)),
)]
pub(crate) async fn remove_relation(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i32>,
) -> Result<Json<ApiRelation>, AppError> {
    decide(&state, &current, id, Decision::Remove).await
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiBan {
    pub id: i64,
    pub user: String,
    pub reason: String,
    pub banner: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// `null` until lifted.
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiNetworkBan {
    pub id: i64,
    /// A CIDR range.
    #[schema(example = "203.0.113.0/24")]
    pub network: String,
    pub reason: String,
    pub banner: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    /// The network can't see the site at all; otherwise it can look but
    /// not change anything.
    pub full: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BanList {
    /// Active user bans, newest first (at most 200).
    pub users: Vec<ApiBan>,
    /// Active network bans.
    pub networks: Vec<ApiNetworkBan>,
}

/// List active bans.
///
/// Needs `ban_users`.
#[utoipa::path(
    get,
    path = "/bans",
    operation_id = "list_bans",
    tag = "moderation",
    responses(
        (status = 200, body = BanList),
        (status = 403, body = ErrorBody, description = "You can't ban users"),
    ),
)]
pub(crate) async fn list_bans(
    State(state): State<AppState>,
    current: CurrentUser,
) -> Result<Json<BanList>, AppError> {
    current.require(Permission::BanUsers)?;
    let db = state.db.primary();
    Ok(Json(BanList {
        users: bans::active(db, 200)
            .await?
            .into_iter()
            .map(|b| ApiBan {
                id: b.id,
                user: b.user_name,
                reason: b.reason,
                banner: b.banner_name,
                created_at: b.created_at,
                expires_at: b.expires_at,
            })
            .collect(),
        networks: bans::active_networks(db)
            .await?
            .into_iter()
            .map(|b| ApiNetworkBan {
                id: b.id,
                network: b.network.to_string(),
                reason: b.reason,
                banner: b.banner_name,
                created_at: b.created_at,
                expires_at: b.expires_at,
                full: b.full,
            })
            .collect(),
    }))
}

fn expiry(days: Option<i64>) -> Result<Option<OffsetDateTime>, AppError> {
    crate::bans::expiry(days).map_err(AppError::Unprocessable)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewBan {
    /// Shown to the user while banned.
    reason: String,
    /// How long, in days; leave out for until lifted.
    days: Option<i64>,
}

/// Ban a user.
///
/// Needs `ban_users` and a role ranked above the user's. Banned users can
/// look around as visitors do, but can't change anything.
#[utoipa::path(
    post,
    path = "/users/{name}/ban",
    operation_id = "ban_user",
    tag = "moderation",
    params(("name" = String, Path, description = "Case-insensitive")),
    request_body = NewBan,
    responses(
        (status = 204, description = "Banned"),
        (status = 403, body = ErrorBody, description = "You can't ban this user"),
    ),
)]
pub(crate) async fn ban_user(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(name): Path<String>,
    Json(ban): Json<NewBan>,
) -> Result<StatusCode, AppError> {
    current.require(Permission::BanUsers)?;
    let expires_at = expiry(ban.days)?;
    crate::bans::ban(&state, &current, &name, &ban.reason, expires_at).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Lift a user's ban.
///
/// Needs `ban_users` and a role ranked above the user's.
#[utoipa::path(
    delete,
    path = "/users/{name}/ban",
    operation_id = "unban_user",
    tag = "moderation",
    params(("name" = String, Path, description = "Case-insensitive")),
    responses(
        (status = 204, description = "Lifted"),
        (status = 400, body = ErrorBody, description = "The user isn't banned"),
    ),
)]
pub(crate) async fn unban_user(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(name): Path<String>,
) -> Result<StatusCode, AppError> {
    crate::bans::unban(&state, &current, &name).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewNetworkBan {
    /// An address, or a range like `203.0.113.0/24`. It may not include
    /// your own address.
    network: String,
    reason: String,
    /// How long, in days; leave out for until lifted.
    days: Option<i64>,
    /// Keep the network from seeing the site at all, rather than only
    /// from changing anything.
    #[serde(default)]
    full: bool,
}

/// Ban a network.
///
/// Needs `ban_users`. Requests from the network can read but not change
/// anything, and it can't register or log in; with `full`, it can't see
/// the site at all.
#[utoipa::path(
    post,
    path = "/network-bans",
    operation_id = "ban_network",
    tag = "moderation",
    request_body = NewNetworkBan,
    responses(
        (status = 201, body = ApiNetworkBan),
        (status = 400, body = ErrorBody, description = "The range isn't valid, is too wide, or includes your address"),
    ),
)]
pub(crate) async fn ban_network(
    State(state): State<AppState>,
    current: CurrentUser,
    info: RequestInfo,
    Json(ban): Json<NewNetworkBan>,
) -> Result<(StatusCode, Json<ApiNetworkBan>), AppError> {
    current.require(Permission::BanUsers)?;
    let expires_at = expiry(ban.days)?;
    let network = crate::bans::ban_network(
        &state,
        &current,
        info.ip,
        &ban.network,
        &ban.reason,
        expires_at,
        ban.full,
    )
    .await?;
    let created = bans::active_networks(state.db.primary())
        .await?
        .into_iter()
        .find(|b| b.network == network)
        .ok_or_else(|| AppError::Internal("a new network ban is missing".into()))?;
    Ok((
        StatusCode::CREATED,
        Json(ApiNetworkBan {
            id: created.id,
            network: created.network.to_string(),
            reason: created.reason,
            banner: created.banner_name,
            created_at: created.created_at,
            expires_at: created.expires_at,
            full: created.full,
        }),
    ))
}

/// Lift a network ban.
///
/// Needs `ban_users`.
#[utoipa::path(
    delete,
    path = "/network-bans/{id}",
    operation_id = "lift_network_ban",
    tag = "moderation",
    params(("id" = i64, Path, description = "Network ban id")),
    responses((status = 204, description = "Lifted"), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn lift_network_ban(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    crate::bans::lift_network(&state, &current, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LogParams {
    /// Only this kind of action (`post.delete`, `user.ban`, …).
    action: Option<String>,
    /// Only actions by this user.
    actor: Option<String>,
    /// Only actions on this post.
    post_id: Option<i64>,
    /// Only actions on this user.
    user: Option<String>,
    /// Entries older than this id: a previous page's `next_before`.
    before: Option<i64>,
    /// Entries from this day on (`YYYY-MM-DD`, UTC).
    since: Option<String>,
    /// Entries up to and including this day (`YYYY-MM-DD`, UTC).
    until: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LogEntry {
    pub id: i64,
    /// Who did it; `null` for the command line.
    pub actor: Option<String>,
    #[schema(example = "post.delete")]
    pub action: String,
    pub post_id: Option<i64>,
    /// The user acted on.
    pub user: Option<String>,
    pub reason: String,
    /// Anything else recorded, depending on the action.
    #[schema(value_type = Object)]
    pub details: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LogPage {
    /// Newest first, 50 per page.
    pub entries: Vec<LogEntry>,
    /// `before` for the next page; absent on the last one.
    pub next_before: Option<i64>,
}

/// Entries per page of the log.
const LOG_PAGE: i64 = 50;

/// Read the moderation log.
///
/// Needs `view_audit_log`.
#[utoipa::path(
    get,
    path = "/moderation/log",
    operation_id = "list_mod_actions",
    tag = "moderation",
    params(LogParams),
    responses(
        (status = 200, body = LogPage),
        (status = 400, body = ErrorBody, description = "Unknown action"),
        (status = 422, body = ErrorBody, description = "A date isn't `YYYY-MM-DD`"),
    ),
)]
pub(crate) async fn log(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<LogParams>,
) -> Result<Json<LogPage>, AppError> {
    current.require(Permission::ViewAuditLog)?;
    let db = state.reader(&current);
    let user_id = async |name: Option<&str>| -> Result<Option<i64>, AppError> {
        match name {
            None => Ok(None),
            // Nobody by that name: match nothing.
            Some(name) => Ok(Some(users::by_name(db, name).await?.map_or(-1, |u| u.id))),
        }
    };
    let action = match params.action.as_deref() {
        None => None,
        Some(name) => Some(
            ActionKind::parse(name)
                .ok_or_else(|| AppError::BadRequest(format!("Unknown action `{name}`")))?,
        ),
    };
    let day = |text: &Option<String>| {
        crate::moderation::parse_day(text.as_deref().unwrap_or("")).map_err(|e| match e {
            AppError::BadRequest(message) => AppError::Unprocessable(message),
            other => other,
        })
    };
    let (since, until) = crate::moderation::day_span(day(&params.since)?, day(&params.until)?);
    let filter = Filter {
        action,
        actor_id: user_id(params.actor.as_deref()).await?,
        post_id: params.post_id,
        user_id: user_id(params.user.as_deref()).await?,
        before: params.before,
        since,
        until,
    };
    let entries = mod_actions::list(db, &filter, LOG_PAGE).await?;
    let next_before = (entries.len() == LOG_PAGE as usize)
        .then(|| entries.last().map(|e| e.id))
        .flatten();
    Ok(Json(LogPage {
        entries: entries
            .into_iter()
            .map(|e: Entry| LogEntry {
                id: e.id,
                actor: e.actor_name,
                action: e.action,
                post_id: e.post_id,
                user: e.user_name,
                reason: e.reason,
                details: e.details,
                created_at: e.created_at,
            })
            .collect(),
        next_before,
    }))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use moekura_db::settings;
    use serde_json::json;
    use sqlx::PgPool;

    use crate::api::test_support::{app, json, upload};
    use crate::test_support::{fixture, session_for};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn deletes_a_users_uploads(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        upload(&app, &alice, &fixture::png(20, 20), "cat").await;
        upload(&app, &alice, &fixture::png(24, 20), "dog").await;
        let url = "/api/v1/users/alice/delete-uploads";
        let reason = Some(json!({"reason": "spam"}));
        assert_eq!(
            app.json("POST", url, Some(&alice), reason.clone())
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.json("POST", url, Some(&moderator), Some(json!({"reason": " "})))
                .await
                .status,
            StatusCode::BAD_REQUEST
        );
        let started = app.json("POST", url, Some(&moderator), reason).await;
        assert_eq!(started.status, StatusCode::ACCEPTED, "{}", started.body);
        let batch = json(&started.body);
        assert_eq!(
            (
                &batch["kind"],
                &batch["user"],
                &batch["status"],
                &batch["total"]
            ),
            (
                &json!("delete"),
                &json!("alice"),
                &json!("queued"),
                &json!(2)
            )
        );
        // The job deletes them (moekura_jobs::posts); here it's queued.
        let progress = json(
            &app.get_json(
                &format!("/api/v1/moderation/post-batches/{}", batch["id"]),
                Some(&moderator),
            )
            .await
            .body,
        );
        assert_eq!(progress["id"], batch["id"]);
        assert_eq!(
            app.get_json(
                &format!("/api/v1/moderation/post-batches/{}", batch["id"]),
                Some(&alice),
            )
            .await
            .status,
            StatusCode::FORBIDDEN
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn purges_deleted_posts_in_bulk(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let first = upload(&app, &alice, &fixture::png(20, 20), "cat").await;
        let second = upload(&app, &alice, &fixture::png(24, 20), "dog").await;
        let delete = |id: i64| format!("/api/v1/posts/{id}/delete");
        app.json(
            "POST",
            &delete(first),
            Some(&admin),
            Some(json!({"reason": "x"})),
        )
        .await;
        let url = "/api/v1/moderation/purge";
        assert_eq!(
            app.json("POST", url, Some(&moderator), Some(json!({"query": ""})))
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        for bad in [json!({}), json!({"query": "", "post_ids": [first]})] {
            assert_eq!(
                app.json("POST", url, Some(&admin), Some(bad)).await.status,
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            app.json(
                "POST",
                url,
                Some(&admin),
                Some(json!({"post_ids": [second]}))
            )
            .await
            .status,
            StatusCode::BAD_REQUEST,
            "not deleted"
        );
        assert_eq!(
            app.json(
                "POST",
                url,
                Some(&admin),
                Some(json!({"query": "status:pending"}))
            )
            .await
            .status,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let started = app
            .json("POST", url, Some(&admin), Some(json!({"query": "cat"})))
            .await;
        assert_eq!(started.status, StatusCode::ACCEPTED, "{}", started.body);
        let batch = json(&started.body);
        assert_eq!(
            (&batch["kind"], &batch["query"], &batch["total"]),
            (&json!("purge"), &json!("cat status:deleted"), &json!(1))
        );
        let follow = format!("/api/v1/moderation/post-batches/{}", batch["id"]);
        assert_eq!(
            app.get_json(&follow, Some(&moderator)).await.status,
            StatusCode::FORBIDDEN,
            "purges are for purgers"
        );
        assert_eq!(
            app.get_json(&follow, Some(&admin)).await.status,
            StatusCode::OK
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn reviews_posts_and_flags(pool: PgPool) {
        settings::set(&pool, "upload_approval", json!(true))
            .await
            .unwrap();
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let first = upload(&app, &alice, &fixture::png(20, 20), "cat").await;
        let second = upload(&app, &alice, &fixture::png(24, 20), "dog").await;
        let post = |id: i64, action: &str| format!("/api/v1/posts/{id}/{action}");

        let refused = app
            .json("POST", &post(first, "approve"), Some(&alice), None)
            .await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        let approved = json(
            &app.json("POST", &post(first, "approve"), Some(&moderator), None)
                .await
                .body,
        );
        assert_eq!(approved["status"], json!("active"));
        let rejected = json(
            &app.json(
                "POST",
                &post(second, "reject"),
                Some(&moderator),
                Some(json!({"reason": "blurry"})),
            )
            .await
            .body,
        );
        assert_eq!(rejected["status"], json!("deleted"));
        let twice = app
            .json("POST", &post(first, "approve"), Some(&moderator), None)
            .await;
        assert_eq!(twice.status, StatusCode::BAD_REQUEST);

        // Flags: raised by members, listed and dismissed by moderators.
        app.json(
            "POST",
            &post(first, "flags"),
            Some(&alice),
            Some(json!({"reason": "off-topic"})),
        )
        .await;
        let open = json(&app.get("/api/v1/flags", Some(&moderator)).await.body);
        assert_eq!(open[0]["post_id"], json!(first));
        assert_eq!(open[0]["flags"][0]["reason"], json!("off-topic"));
        let dismissed = json(
            &app.json(
                "POST",
                &post(first, "flags/dismiss"),
                Some(&moderator),
                None,
            )
            .await
            .body,
        );
        assert_eq!(dismissed, json!({"dismissed": 1}));
        assert_eq!(
            json(&app.get("/api/v1/flags", Some(&moderator)).await.body),
            json!([])
        );
        assert_eq!(
            app.get("/api/v1/flags", Some(&alice)).await.status,
            StatusCode::FORBIDDEN
        );

        let deleted = json(
            &app.json(
                "POST",
                &post(first, "delete"),
                Some(&moderator),
                Some(json!({"reason": "dupe"})),
            )
            .await
            .body,
        );
        assert_eq!(deleted["status"], json!("deleted"));
        let restored = json(
            &app.json("POST", &post(first, "restore"), Some(&moderator), None)
                .await
                .body,
        );
        assert_eq!(restored["status"], json!("active"));
        // Purging needs a deleted post, and an admin.
        let active = app
            .json("POST", &post(first, "purge"), Some(&admin), None)
            .await;
        assert_eq!(active.status, StatusCode::BAD_REQUEST);
        let queued = app
            .json("POST", &post(second, "purge"), Some(&admin), None)
            .await;
        assert_eq!(queued.status, StatusCode::ACCEPTED, "{}", queued.body);

        let log = json(
            &app.get(
                "/api/v1/moderation/log?action=post.reject",
                Some(&moderator),
            )
            .await
            .body,
        );
        let entry = &log["entries"][0];
        assert_eq!(
            (&entry["actor"], &entry["post_id"], &entry["reason"]),
            (&json!("mod"), &json!(second), &json!("blurry"))
        );
        assert_eq!(log["next_before"], json!(null));
        let by_mod = json(
            &app.get("/api/v1/moderation/log?actor=mod", Some(&moderator))
                .await
                .body,
        );
        assert!(by_mod["entries"].as_array().unwrap().len() >= 5, "{by_mod}");
        let unknown = app
            .get("/api/v1/moderation/log?action=nope", Some(&moderator))
            .await;
        assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            app.get("/api/v1/moderation/log", Some(&alice)).await.status,
            StatusCode::FORBIDDEN
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn decides_relations(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let mut ids = Vec::new();
        for (a, c) in [("kitty", "cat"), ("doggo", "dog"), ("birb", "bird")] {
            let created = app
                .json(
                    "POST",
                    "/api/v1/tag-relations",
                    Some(&alice),
                    Some(json!({"kind": "alias", "antecedent": a, "consequent": c})),
                )
                .await;
            ids.push(json(&created.body)["id"].as_i64().unwrap());
        }
        let path = |id: i64, rest: &str| format!("/api/v1/tag-relations/{id}{rest}");

        let refused = app
            .json("POST", &path(ids[0], "/approve"), Some(&alice), None)
            .await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        let approved = json(
            &app.json("POST", &path(ids[0], "/approve"), Some(&admin), None)
                .await
                .body,
        );
        assert_eq!(approved["status"], json!("active"));
        let rejected = json(
            &app.json("POST", &path(ids[1], "/reject"), Some(&admin), None)
                .await
                .body,
        );
        assert_eq!(rejected["status"], json!("rejected"));
        // Creators may withdraw their pending requests.
        let withdrawn = json(
            &app.json("DELETE", &path(ids[2], ""), Some(&alice), None)
                .await
                .body,
        );
        assert_eq!(withdrawn["status"], json!("deleted"));
        let active = app
            .json("DELETE", &path(ids[0], ""), Some(&alice), None)
            .await;
        assert_eq!(active.status, StatusCode::FORBIDDEN);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn bans_users_and_networks(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        session_for(&pool, "root", SystemRole::Admin).await;

        let ban = json!({"reason": "spam", "days": 7});
        let banned = app
            .json(
                "POST",
                "/api/v1/users/alice/ban",
                Some(&moderator),
                Some(ban.clone()),
            )
            .await;
        assert_eq!(banned.status, StatusCode::NO_CONTENT, "{}", banned.body);
        let me = json(&app.get("/api/v1/me", Some(&alice)).await.body);
        assert_eq!(me["ban"]["reason"], json!("spam"));
        let list = json(&app.get("/api/v1/bans", Some(&moderator)).await.body);
        assert_eq!(list["users"][0]["user"], json!("alice"));
        assert!(list["users"][0]["expires_at"].is_string());
        // Banning again replaces the ban rather than stacking another.
        let longer = app
            .json(
                "POST",
                "/api/v1/users/alice/ban",
                Some(&moderator),
                Some(json!({"reason": "more spam"})),
            )
            .await;
        assert_eq!(longer.status, StatusCode::NO_CONTENT, "{}", longer.body);
        let list = json(&app.get("/api/v1/bans", Some(&moderator)).await.body);
        assert_eq!(list["users"].as_array().unwrap().len(), 1);
        assert_eq!(list["users"][0]["reason"], json!("more spam"));
        assert!(list["users"][0]["expires_at"].is_null());

        // Only lower ranks, and within limits.
        let admin = app
            .json(
                "POST",
                "/api/v1/users/root/ban",
                Some(&moderator),
                Some(ban),
            )
            .await;
        assert_eq!(admin.status, StatusCode::FORBIDDEN);
        let forever_and_more = app
            .json(
                "POST",
                "/api/v1/users/alice/ban",
                Some(&moderator),
                Some(json!({"reason": "x", "days": 100000})),
            )
            .await;
        assert_eq!(forever_and_more.status, StatusCode::UNPROCESSABLE_ENTITY);

        let lifted = app
            .json("DELETE", "/api/v1/users/alice/ban", Some(&moderator), None)
            .await;
        assert_eq!(lifted.status, StatusCode::NO_CONTENT);
        let again = app
            .json("DELETE", "/api/v1/users/alice/ban", Some(&moderator), None)
            .await;
        assert_eq!(again.status, StatusCode::BAD_REQUEST);

        let network = app
            .json(
                "POST",
                "/api/v1/network-bans",
                Some(&moderator),
                Some(json!({"network": "203.0.113.7/24", "reason": "abuse"})),
            )
            .await;
        assert_eq!(network.status, StatusCode::CREATED, "{}", network.body);
        let network = json(&network.body);
        assert_eq!(network["network"], json!("203.0.113.0/24"));
        assert_eq!(network["full"], json!(false));
        let too_wide = app
            .json(
                "POST",
                "/api/v1/network-bans",
                Some(&moderator),
                Some(json!({"network": "10.0.0.0/4", "reason": "x"})),
            )
            .await;
        assert_eq!(too_wide.status, StatusCode::BAD_REQUEST);
        let path = format!("/api/v1/network-bans/{}", network["id"]);
        assert_eq!(
            app.json("DELETE", &path, Some(&moderator), None)
                .await
                .status,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            app.json("DELETE", &path, Some(&moderator), None)
                .await
                .status,
            StatusCode::NOT_FOUND
        );
    }
}
