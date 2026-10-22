//! The login sessions repository, against a real MariaDB.

mod common;

use chrono::{DateTime, Duration, SubsecRound, Utc};
use haloid_core::account::{User, UserStatus};
use haloid_core::auth::Role;
use haloid_db::sessions::{self, NewSession};
use haloid_db::users::{self, NewUser};
use std::net::IpAddr;

fn now() -> DateTime<Utc> {
    Utc::now().trunc_subsecs(0)
}

async fn user(db: &haloid_db::Db, name: &str) -> User {
    users::create(
        db,
        &NewUser {
            username: name,
            email: None,
            password_hash: None,
            role: Role::User,
            status: UserStatus::Valid,
        },
    )
    .await
    .expect("create user")
}

fn session<'a>(user: &User, token_hash: &'a str, expires_at: DateTime<Utc>) -> NewSession<'a> {
    NewSession {
        user_id: user.id,
        token_hash,
        ip: None,
        user_agent: None,
        expires_at,
    }
}

/// A distinct, well-formed sha256 hex digest per label.
fn hash(label: char) -> String {
    label.to_string().repeat(64)
}

#[tokio::test]
async fn an_active_session_is_found_by_its_token_hash_with_its_user() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let expires = now() + Duration::days(30);

    let ip: IpAddr = "2001:db8::7".parse().unwrap();
    let created = sessions::create(
        &scratch.pool,
        &NewSession {
            ip: Some(ip),
            user_agent: Some("ShareX/17.0"),
            ..session(&alice, &hash('a'), expires)
        },
    )
    .await
    .expect("create session");
    assert_eq!(created.id.as_str().len(), 36, "a UUID");
    assert_eq!(created.user_id, alice.id);
    assert_eq!(created.ip, Some(ip));
    assert_eq!(created.user_agent.as_deref(), Some("ShareX/17.0"));
    assert_eq!(created.expires_at, expires);

    let (found, owner) = sessions::find_active(&scratch.pool, &hash('a'), now())
        .await
        .unwrap()
        .expect("the session is active");
    assert_eq!(found, created);
    assert_eq!(owner, alice);

    assert!(
        sessions::find_active(&scratch.pool, &hash('b'), now())
            .await
            .unwrap()
            .is_none(),
        "an unknown token matched"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn an_expired_session_is_not_active() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let expires = now() + Duration::hours(1);
    sessions::create(&scratch.pool, &session(&alice, &hash('a'), expires))
        .await
        .unwrap();

    let before = expires - Duration::seconds(1);
    assert!(
        sessions::find_active(&scratch.pool, &hash('a'), before)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        sessions::find_active(&scratch.pool, &hash('a'), expires)
            .await
            .unwrap()
            .is_none(),
        "a session was still active at its expiry instant"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn an_overlong_user_agent_is_truncated_not_rejected() {
    // The column is VARCHAR(255); strict mode would fail the insert, and a
    // login must not fail because of a browser's header.
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let long = "é".repeat(400);

    let created = sessions::create(
        &scratch.pool,
        &NewSession {
            user_agent: Some(&long),
            ..session(&alice, &hash('a'), now() + Duration::hours(1))
        },
    )
    .await
    .expect("create with a long user agent");
    assert_eq!(created.user_agent.map(|ua| ua.chars().count()), Some(255));

    scratch.drop_db().await;
}

#[tokio::test]
async fn touching_a_session_moves_last_seen() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let created = sessions::create(
        &scratch.pool,
        &session(&alice, &hash('a'), now() + Duration::days(1)),
    )
    .await
    .unwrap();

    let later = now() + Duration::minutes(10);
    assert!(
        sessions::touch(&scratch.pool, &created.id, later)
            .await
            .unwrap()
    );

    let (found, _) = sessions::find_active(&scratch.pool, &hash('a'), now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.last_seen_at, later);

    scratch.drop_db().await;
}

#[tokio::test]
async fn logging_out_everywhere_else_keeps_the_current_session() {
    // What a password change does: every other session dies, this one stays.
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let bob = user(&scratch.pool, "bob").await;
    let expires = now() + Duration::days(1);

    let current = sessions::create(&scratch.pool, &session(&alice, &hash('a'), expires))
        .await
        .unwrap();
    for label in ['b', 'c'] {
        sessions::create(&scratch.pool, &session(&alice, &hash(label), expires))
            .await
            .unwrap();
    }
    sessions::create(&scratch.pool, &session(&bob, &hash('d'), expires))
        .await
        .unwrap();

    let removed = sessions::delete_for_user(&scratch.pool, alice.id, Some(&current.id))
        .await
        .unwrap();
    assert_eq!(removed, 2);

    let left = sessions::list_for_user(&scratch.pool, alice.id)
        .await
        .unwrap();
    assert_eq!(
        left.iter().map(|s| &s.id).collect::<Vec<_>>(),
        vec![&current.id]
    );
    assert_eq!(
        sessions::list_for_user(&scratch.pool, bob.id)
            .await
            .unwrap()
            .len(),
        1,
        "another user's session was removed"
    );

    assert_eq!(
        sessions::delete_for_user(&scratch.pool, alice.id, None)
            .await
            .unwrap(),
        1
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn deleting_one_session_reports_whether_it_existed() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let created = sessions::create(
        &scratch.pool,
        &session(&alice, &hash('a'), now() + Duration::days(1)),
    )
    .await
    .unwrap();

    assert!(sessions::delete(&scratch.pool, &created.id).await.unwrap());
    assert!(!sessions::delete(&scratch.pool, &created.id).await.unwrap());

    scratch.drop_db().await;
}

#[tokio::test]
async fn the_expiry_sweep_removes_only_expired_sessions() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let at = now();

    sessions::create(
        &scratch.pool,
        &session(&alice, &hash('a'), at - Duration::hours(1)),
    )
    .await
    .unwrap();
    sessions::create(&scratch.pool, &session(&alice, &hash('b'), at))
        .await
        .unwrap();
    let live = sessions::create(
        &scratch.pool,
        &session(&alice, &hash('c'), at + Duration::hours(1)),
    )
    .await
    .unwrap();

    assert_eq!(
        sessions::delete_expired(&scratch.pool, at).await.unwrap(),
        2
    );
    let left = sessions::list_for_user(&scratch.pool, alice.id)
        .await
        .unwrap();
    assert_eq!(left, vec![live]);

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_users_sessions_go_with_the_user() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    sessions::create(
        &scratch.pool,
        &session(&alice, &hash('a'), now() + Duration::days(1)),
    )
    .await
    .unwrap();

    sqlx::query("DELETE FROM ih_users WHERE id = ?")
        .bind(alice.id.0)
        .execute(&scratch.pool)
        .await
        .unwrap();

    assert!(
        sessions::find_active(&scratch.pool, &hash('a'), now())
            .await
            .unwrap()
            .is_none(),
        "a deleted user's session still authenticates"
    );

    scratch.drop_db().await;
}
