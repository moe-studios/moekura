use moekura_core::totp::Secret;
use moekura_db::two_factor;
use serde_json::Value;
use sqlx::PgPool;

use super::authenticator::Authenticator;
use super::*;
use crate::test_support::{TestApp, TestResponse, member, test_config, test_state};

const ORIGIN: &str = "http://localhost:8080";

/// Turns on two-factor login for `user_id`.
async fn turn_on_two_factor(pool: &PgPool, user_id: i64) {
    two_factor::begin(pool, user_id, &Secret::generate())
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    two_factor::enable(&mut conn, user_id, 0, &[])
        .await
        .unwrap();
}

/// Logs alice in with her password, up to the code; returns the cookie
/// that holds the login waiting for it.
async fn password(app: &TestApp) -> String {
    let first = app
        .post_form(
            "/login",
            None,
            &[],
            "name=alice&password=correct+horse&next=%2Ftags",
        )
        .await;
    assert_eq!(first.location.as_deref(), Some("/login/code"));
    first
        .set_cookie
        .iter()
        .find_map(|c| c.strip_prefix("moekura_login="))
        .and_then(|rest| rest.split(';').next())
        .map(|token| format!("moekura_login={token}"))
        .expect("a login challenge cookie")
}

async fn app(pool: &PgPool) -> TestApp {
    TestApp::new(
        test_state(pool).await,
        routes()
            .merge(crate::account::routes())
            .merge(crate::email::routes())
            .merge(crate::two_factor::routes())
            .merge(crate::posts::routes()),
    )
}

fn json(response: &TestResponse) -> Value {
    serde_json::from_str(&response.body).unwrap_or_else(|_| panic!("JSON: {}", response.body))
}

/// Adds a passkey called `name` to the account logged in as `session`.
async fn add(app: &TestApp, session: &str, authenticator: &mut Authenticator, name: &str) {
    let asked = app
        .json(
            "POST",
            "/settings/passkeys/options",
            Some(session),
            Some(serde_json::json!({ "name": name, "password": "correct horse" })),
        )
        .await;
    assert_eq!(asked.status, StatusCode::OK, "{}", asked.body);
    let asked = json(&asked);
    let credential = authenticator.register(&asked["options"]);
    let added = app
        .json(
            "POST",
            "/settings/passkeys",
            Some(session),
            Some(serde_json::json!({ "token": asked["token"], "credential": credential })),
        )
        .await;
    assert_eq!(added.status, StatusCode::OK, "{}", added.body);
    assert_eq!(json(&added)["redirect"], ACCOUNT_PAGE);
}

/// Logs in with passkey `index`, without a password.
async fn log_in(app: &TestApp, authenticator: &mut Authenticator, index: usize) -> TestResponse {
    let asked = app.json("POST", "/login/passkey/options", None, None).await;
    assert_eq!(asked.status, StatusCode::OK, "{}", asked.body);
    let asked = json(&asked);
    let credential = authenticator.sign(&asked["options"], index);
    app.json(
        "POST",
        "/login/passkey",
        None,
        Some(serde_json::json!({
            "token": asked["token"],
            "credential": credential,
            "next": "/tags",
        })),
    )
    .await
}

#[sqlx::test(migrator = "moekura_db::MIGRATOR")]
async fn adding_one_and_logging_in_without_a_password(pool: PgPool) {
    let app = app(&pool).await;
    let (alice, session) = member(&pool, "alice", "alice@example.com").await;
    let mut phone = Authenticator::new(ORIGIN);

    let account = app.get("/settings/account", Some(&session)).await;
    assert!(
        account.body.contains("data-passkey-register"),
        "{}",
        account.body
    );

    // The password first.
    let wrong = app
        .json(
            "POST",
            "/settings/passkeys/options",
            Some(&session),
            Some(serde_json::json!({ "name": "Phone", "password": "nope" })),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json(&wrong)["error"], "Wrong password.");
    let unnamed = app
        .json(
            "POST",
            "/settings/passkeys/options",
            Some(&session),
            Some(serde_json::json!({ "name": "  ", "password": "correct horse" })),
        )
        .await;
    assert_eq!(unnamed.status, StatusCode::UNPROCESSABLE_ENTITY);

    let asked = app
        .json(
            "POST",
            "/settings/passkeys/options",
            Some(&session),
            Some(serde_json::json!({ "name": " Phone ", "password": "correct horse" })),
        )
        .await;
    let asked = json(&asked);
    let options = &asked["options"]["publicKey"];
    assert_eq!(options["rp"]["id"], "localhost");
    assert_eq!(options["user"]["name"], "alice");
    // Discoverable where it can be, so it can log in without a name.
    assert_eq!(
        options["authenticatorSelection"]["residentKey"],
        "preferred"
    );
    let credential = phone.register(&asked["options"]);
    let answer = serde_json::json!({ "token": asked["token"], "credential": credential });
    let added = app
        .json(
            "POST",
            "/settings/passkeys",
            Some(&session),
            Some(answer.clone()),
        )
        .await;
    assert_eq!(added.status, StatusCode::OK, "{}", added.body);
    // The answer can't be sent twice.
    let again = app
        .json("POST", "/settings/passkeys", Some(&session), Some(answer))
        .await;
    assert_eq!(again.status, StatusCode::UNPROCESSABLE_ENTITY);

    let saved = stored::for_user(&pool, alice.id).await.unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "Phone");
    assert_eq!(saved[0].credential_id, phone.credentials[0].id);
    let account = app.get("/settings/account", Some(&session)).await;
    assert!(account.body.contains("Phone"), "{}", account.body);

    let done = log_in(&app, &mut phone, 0).await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    assert_eq!(json(&done)["redirect"], "/tags");
    let new_session = done.session_cookie().expect("logged in");
    let state = test_state(&pool).await;
    let me = crate::test_support::current_user(&state, &new_session).await;
    assert_eq!(me.user.map(|u| u.id), Some(alice.id));
    let used = stored::for_user(&pool, alice.id).await.unwrap();
    assert!(used[0].last_used_at.is_some());
    assert_eq!(
        used[0].credential["cred"]["counter"], 1,
        "{}",
        used[0].credential
    );

    // A second passkey shares the user handle, and the first isn't made
    // again on the same device.
    let asked = app
        .json(
            "POST",
            "/settings/passkeys/options",
            Some(&session),
            Some(serde_json::json!({ "name": "Key", "password": "correct horse" })),
        )
        .await;
    let excluded = &json(&asked)["options"]["publicKey"]["excludeCredentials"];
    assert_eq!(excluded.as_array().map(Vec::len), Some(1), "{excluded}");
    let mut key = Authenticator::new(ORIGIN);
    add(&app, &session, &mut key, "Key").await;
    let both = stored::for_user(&pool, alice.id).await.unwrap();
    assert_eq!(both[0].user_handle, both[1].user_handle);
    assert_eq!(log_in(&app, &mut key, 0).await.status, StatusCode::OK);
}

#[sqlx::test(migrator = "moekura_db::MIGRATOR")]
async fn a_sign_count_going_back_is_refused(pool: PgPool) {
    let app = app(&pool).await;
    let (_, session) = member(&pool, "alice", "alice@example.com").await;
    let mut phone = Authenticator::new(ORIGIN);
    add(&app, &session, &mut phone, "Phone").await;
    for _ in 0..3 {
        assert_eq!(log_in(&app, &mut phone, 0).await.status, StatusCode::OK);
    }
    // As if a copy of the passkey had been used less.
    phone.credentials[0].counter = 1;
    let cloned = log_in(&app, &mut phone, 0).await;
    assert_eq!(cloned.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(cloned.session_cookie().is_none());
    assert!(
        json(&cloned)["error"]
            .as_str()
            .unwrap()
            .contains("didn't work")
    );
}

#[sqlx::test(migrator = "moekura_db::MIGRATOR")]
async fn a_signed_answer_works_once_and_for_its_own_challenge(pool: PgPool) {
    let app = app(&pool).await;
    let (_, session) = member(&pool, "alice", "alice@example.com").await;
    let mut phone = Authenticator::new(ORIGIN);
    add(&app, &session, &mut phone, "Phone").await;

    let first = json(&app.json("POST", "/login/passkey/options", None, None).await);
    let second = json(&app.json("POST", "/login/passkey/options", None, None).await);
    let signed = phone.sign(&first["options"], 0);
    // Signed for the first challenge, sent with the second.
    let swapped = app
        .json(
            "POST",
            "/login/passkey",
            None,
            Some(serde_json::json!({ "token": second["token"], "credential": signed })),
        )
        .await;
    assert_eq!(swapped.status, StatusCode::UNPROCESSABLE_ENTITY);
    let right = serde_json::json!({ "token": first["token"], "credential": signed });
    let done = app
        .json("POST", "/login/passkey", None, Some(right.clone()))
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    // Without a page to go back to, home.
    assert_eq!(json(&done)["redirect"], "/");
    let replayed = app.json("POST", "/login/passkey", None, Some(right)).await;
    assert_eq!(replayed.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(replayed.session_cookie().is_none());
}

#[sqlx::test(migrator = "moekura_db::MIGRATOR")]
async fn renaming_and_removing(pool: PgPool) {
    let app = app(&pool).await;
    let (alice, session) = member(&pool, "alice", "alice@example.com").await;
    let (_, bob) = member(&pool, "bob", "bob@example.com").await;
    let mut phone = Authenticator::new(ORIGIN);
    add(&app, &session, &mut phone, "Phone").await;
    let id = stored::for_user(&pool, alice.id).await.unwrap()[0].id;

    let renamed = app
        .post_form(
            "/settings/passkeys/rename",
            Some(&session),
            &[],
            &format!("id={id}&name=Work+phone"),
        )
        .await;
    assert_eq!(renamed.location.as_deref(), Some(ACCOUNT_PAGE));
    let blank = app
        .post_form(
            "/settings/passkeys/rename",
            Some(&session),
            &[],
            &format!("id={id}&name="),
        )
        .await;
    assert_eq!(blank.status, StatusCode::UNPROCESSABLE_ENTITY);
    // Only one's own.
    let theirs = app
        .post_form(
            "/settings/passkeys/rename",
            Some(&bob),
            &[],
            &format!("id={id}&name=Mine"),
        )
        .await;
    assert_eq!(theirs.status, StatusCode::NOT_FOUND);
    let gone = app
        .post_form(
            "/settings/passkeys/remove",
            Some(&bob),
            &[],
            &format!("id={id}&password=correct+horse"),
        )
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    assert_eq!(
        stored::for_user(&pool, alice.id).await.unwrap()[0].name,
        "Work phone"
    );

    let wrong = app
        .post_form(
            "/settings/passkeys/remove",
            Some(&session),
            &[],
            &format!("id={id}&password=nope"),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(wrong.body.contains("Wrong password."), "{}", wrong.body);
    assert!(stored::has_any(&pool, alice.id).await.unwrap());
    let removed = app
        .post_form(
            "/settings/passkeys/remove",
            Some(&session),
            &[],
            &format!("id={id}&password=correct+horse"),
        )
        .await;
    assert_eq!(removed.location.as_deref(), Some(ACCOUNT_PAGE));
    assert!(!stored::has_any(&pool, alice.id).await.unwrap());

    // It doesn't log in any more.
    let refused = log_in(&app, &mut phone, 0).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        json(&refused)["error"]
            .as_str()
            .unwrap()
            .contains("isn't registered")
    );
}

#[sqlx::test(migrator = "moekura_db::MIGRATOR")]
async fn instead_of_a_code_after_the_password(pool: PgPool) {
    let app = app(&pool).await;
    let (alice, session) = member(&pool, "alice", "alice@example.com").await;
    turn_on_two_factor(&pool, alice.id).await;
    let mut phone = Authenticator::new(ORIGIN);

    // Without a passkey, the code page doesn't offer one.
    let cookie = password(&app).await;
    let page = app.get_with_cookie("/login/code", &cookie).await;
    assert!(!page.body.contains("data-passkey-second-factor"));
    let none = app
        .json_with_cookie("/login/code/passkey/options", &cookie, Value::Null)
        .await;
    assert_eq!(none.status, StatusCode::UNPROCESSABLE_ENTITY);

    add(&app, &session, &mut phone, "Phone").await;
    let cookie = password(&app).await;
    let page = app.get_with_cookie("/login/code", &cookie).await;
    assert!(
        page.body.contains("data-passkey-second-factor"),
        "{}",
        page.body
    );
    let asked = app
        .json_with_cookie("/login/code/passkey/options", &cookie, Value::Null)
        .await;
    assert_eq!(asked.status, StatusCode::OK, "{}", asked.body);
    let asked = json(&asked);
    // Only her passkeys.
    let allowed = &asked["options"]["publicKey"]["allowCredentials"];
    assert_eq!(allowed.as_array().map(Vec::len), Some(1), "{allowed}");
    let credential = phone.sign(&asked["options"], 0);
    let done = app
        .json_with_cookie(
            "/login/code/passkey",
            &cookie,
            serde_json::json!({ "token": asked["token"], "credential": credential }),
        )
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    assert_eq!(json(&done)["redirect"], "/tags");
    assert!(done.session_cookie().is_some());
    assert!(
        done.set_cookie
            .iter()
            .any(|c| c.starts_with("moekura_login=") && c.contains("Max-Age=0")),
        "{:?}",
        done.set_cookie
    );
    // The login it finished is over.
    let over = app
        .json_with_cookie("/login/code/passkey/options", &cookie, Value::Null)
        .await;
    assert_eq!(over.status, StatusCode::CONFLICT);
    assert_eq!(json(&over)["redirect"], "/login");

    // A passkey answer for one step can't finish the other.
    let cookie = password(&app).await;
    let asked = json(
        &app.json_with_cookie("/login/code/passkey/options", &cookie, Value::Null)
            .await,
    );
    let credential = phone.sign(&asked["options"], 0);
    let crossed = app
        .json(
            "POST",
            "/login/passkey",
            None,
            Some(serde_json::json!({ "token": asked["token"], "credential": credential })),
        )
        .await;
    assert_eq!(crossed.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(crossed.session_cookie().is_none());
}

#[sqlx::test(migrator = "moekura_db::MIGRATOR")]
async fn too_many_tries_at_the_second_step_start_over(pool: PgPool) {
    let app = app(&pool).await;
    let (alice, session) = member(&pool, "alice", "alice@example.com").await;
    turn_on_two_factor(&pool, alice.id).await;
    let mut phone = Authenticator::new(ORIGIN);
    add(&app, &session, &mut phone, "Phone").await;
    let cookie = password(&app).await;
    let asked = json(
        &app.json_with_cookie("/login/code/passkey/options", &cookie, Value::Null)
            .await,
    );
    let credential = phone.sign(&asked["options"], 0);
    for _ in 0..crate::two_factor::MAX_ATTEMPTS {
        let wrong = app
            .json_with_cookie(
                "/login/code/passkey",
                &cookie,
                serde_json::json!({ "token": "nope", "credential": credential }),
            )
            .await;
        assert_eq!(
            wrong.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            wrong.body
        );
    }
    let asked = json(
        &app.json_with_cookie("/login/code/passkey/options", &cookie, Value::Null)
            .await,
    );
    let credential = phone.sign(&asked["options"], 0);
    let late = app
        .json_with_cookie(
            "/login/code/passkey",
            &cookie,
            serde_json::json!({ "token": asked["token"], "credential": credential }),
        )
        .await;
    assert_eq!(late.status, StatusCode::CONFLICT, "{}", late.body);
    assert!(late.session_cookie().is_none());
}

#[sqlx::test(migrator = "moekura_db::MIGRATOR")]
async fn passkey_logins_count_against_the_login_limit(pool: PgPool) {
    let app = app(&pool).await;
    let (_, session) = member(&pool, "alice", "alice@example.com").await;
    let mut phone = Authenticator::new(ORIGIN);
    add(&app, &session, &mut phone, "Phone").await;
    let mut statuses = Vec::new();
    for _ in 0..6 {
        statuses.push(log_in(&app, &mut phone, 0).await.status);
    }
    assert_eq!(&statuses[..5], &[StatusCode::OK; 5]);
    assert_eq!(statuses[5], StatusCode::TOO_MANY_REQUESTS);
}

#[sqlx::test(migrator = "moekura_db::MIGRATOR")]
async fn starting_passkey_logins_is_limited_per_network(pool: PgPool) {
    let peer = std::net::SocketAddr::new(crate::shared::tests::unique_ip(), 443);
    let app = TestApp::with_peer(test_state(&pool).await, routes(), peer);
    for _ in 0..30 {
        let asked = app.json("POST", "/login/passkey/options", None, None).await;
        assert_eq!(asked.status, StatusCode::OK, "{}", asked.body);
    }
    let late = app.json("POST", "/login/passkey/options", None, None).await;
    assert_eq!(late.status, StatusCode::TOO_MANY_REQUESTS);
    // Refused before storing a challenge.
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM passkey_challenges")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, 30);
}

#[sqlx::test(migrator = "moekura_db::MIGRATOR")]
async fn a_passkey_removed_during_the_second_step_doesnt_finish_it(pool: PgPool) {
    let app = app(&pool).await;
    let (alice, session) = member(&pool, "alice", "alice@example.com").await;
    turn_on_two_factor(&pool, alice.id).await;
    let mut phone = Authenticator::new(ORIGIN);
    add(&app, &session, &mut phone, "Phone").await;
    let cookie = password(&app).await;
    let asked = json(
        &app.json_with_cookie("/login/code/passkey/options", &cookie, Value::Null)
            .await,
    );
    let credential = phone.sign(&asked["options"], 0);
    let id = stored::for_user(&pool, alice.id).await.unwrap()[0].id;
    assert!(stored::remove(&pool, alice.id, id).await.unwrap());
    let done = app
        .json_with_cookie(
            "/login/code/passkey",
            &cookie,
            serde_json::json!({ "token": asked["token"], "credential": credential }),
        )
        .await;
    assert_eq!(
        done.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        done.body
    );
    assert!(done.session_cookie().is_none());
}

#[sqlx::test(migrator = "moekura_db::MIGRATOR")]
async fn deactivated_accounts_cant_log_in_with_one(pool: PgPool) {
    let app = app(&pool).await;
    let (alice, session) = member(&pool, "alice", "alice@example.com").await;
    let mut phone = Authenticator::new(ORIGIN);
    add(&app, &session, &mut phone, "Phone").await;
    users::set_status(&pool, alice.id, UserStatus::Deactivated)
        .await
        .unwrap();
    let refused = log_in(&app, &mut phone, 0).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(&refused)["error"],
        "This account has been deactivated."
    );
    assert!(refused.session_cookie().is_none());
}

#[sqlx::test(migrator = "moekura_db::MIGRATOR")]
async fn api_keys_cant_add_passkeys(pool: PgPool) {
    let app = app(&pool).await;
    let (alice, _) = member(&pool, "alice", "alice@example.com").await;
    let key = moekura_db::api_keys::create(&pool, alice.id, "bot", None)
        .await
        .unwrap();
    let response = app
        .raw(
            axum::http::Request::post("/settings/passkeys/options")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {key}"))
                .body(axum::body::Body::from(
                    serde_json::json!({ "name": "Bot", "password": "correct horse" }).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "moekura_db::MIGRATOR")]
async fn sites_at_an_address_have_none(pool: PgPool) {
    assert!(Passkeys::new(&Url::parse("http://127.0.0.1:8080").unwrap()).is_none());
    assert!(Passkeys::new(&Url::parse("http://[::1]:8080").unwrap()).is_none());
    assert!(Passkeys::new(&Url::parse("https://booru.example.com").unwrap()).is_some());

    let mut config = test_config();
    config.server.public_url = Url::parse("http://127.0.0.1:8080").unwrap();
    let app = TestApp::new(
        crate::test_support::test_state_with(&pool, config).await,
        routes()
            .merge(crate::account::routes())
            .merge(crate::email::routes()),
    );
    let (_, session) = member(&pool, "alice", "alice@example.com").await;
    let asked = app.json("POST", "/login/passkey/options", None, None).await;
    assert_eq!(asked.status, StatusCode::NOT_FOUND);
    let account = app.get("/settings/account", Some(&session)).await;
    assert!(!account.body.contains("data-passkey-register"));
    let login = app.get("/login", None).await;
    assert!(!login.body.contains("data-passkey-login"));
}
