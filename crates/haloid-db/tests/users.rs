//! The users repository, against a real MariaDB.

mod common;

use chrono::{DateTime, Duration, SubsecRound, Utc};
use haloid_core::account::UserStatus;
use haloid_core::auth::Role;
use haloid_db::DbError;
use haloid_db::users::{self, NewUser};

/// Whole seconds: `DATETIME` columns hold no fractional part.
fn now() -> DateTime<Utc> {
    Utc::now().trunc_subsecs(0)
}

fn alice() -> NewUser<'static> {
    NewUser {
        username: "Alice",
        email: Some("Alice@Example.com"),
        password_hash: Some("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"),
        role: Role::User,
        status: UserStatus::Valid,
    }
}

#[tokio::test]
async fn a_created_user_reads_back_with_its_defaults() {
    let scratch = migrated_or_skip!();

    let created = users::create(&scratch.pool, &alice())
        .await
        .expect("create");
    assert_eq!(created.username, "Alice", "the typed spelling is kept");
    assert_eq!(created.email.as_deref(), Some("Alice@Example.com"));
    assert_eq!(created.role, Role::User);
    assert_eq!(created.status, UserStatus::Valid);
    assert_eq!(created.id_encoded.as_str().len(), haloid_id::PUBLIC_ID_LEN);
    assert_eq!(created.file_count, 0);
    assert_eq!(created.disk_used, 0);
    assert!(created.locked_until.is_none());
    assert!(created.last_login_at.is_none());

    let by_id = users::find_by_id(&scratch.pool, created.id)
        .await
        .expect("find_by_id");
    assert_eq!(by_id.as_ref(), Some(&created));

    let by_public = users::find_by_id_encoded(&scratch.pool, created.id_encoded.as_str())
        .await
        .expect("find_by_id_encoded");
    assert_eq!(by_public, Some(created));

    scratch.drop_db().await;
}

#[tokio::test]
async fn missing_users_are_none_not_errors() {
    let scratch = migrated_or_skip!();

    assert_eq!(
        users::find_by_id(&scratch.pool, 999.into()).await.unwrap(),
        None
    );
    assert_eq!(
        users::find_by_username(&scratch.pool, "nobody")
            .await
            .unwrap(),
        None
    );
    assert!(
        users::find_for_login(&scratch.pool, "nobody@example.com")
            .await
            .unwrap()
            .is_none()
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn username_lookup_ignores_case() {
    let scratch = migrated_or_skip!();
    let created = users::create(&scratch.pool, &alice()).await.unwrap();

    for spelling in ["alice", "ALICE", "aLiCe"] {
        let found = users::find_by_username(&scratch.pool, spelling)
            .await
            .unwrap();
        assert_eq!(found.map(|u| u.id), Some(created.id), "{spelling:?}");
    }

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_username_differing_only_in_case_is_a_conflict() {
    let scratch = migrated_or_skip!();
    users::create(&scratch.pool, &alice()).await.unwrap();

    let again = NewUser {
        username: "ALICE",
        email: None,
        ..alice()
    };
    match users::create(&scratch.pool, &again).await {
        Err(DbError::Conflict(field)) => assert_eq!(field, "username"),
        other => panic!("expected a username conflict, got {other:?}"),
    }

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_reused_email_is_a_conflict() {
    let scratch = migrated_or_skip!();
    users::create(&scratch.pool, &alice()).await.unwrap();

    let other = NewUser {
        username: "bob",
        email: Some("alice@example.COM"),
        ..alice()
    };
    match users::create(&scratch.pool, &other).await {
        Err(DbError::Conflict(field)) => assert_eq!(field, "email"),
        other => panic!("expected an email conflict, got {other:?}"),
    }

    scratch.drop_db().await;
}

#[tokio::test]
async fn any_number_of_users_may_have_no_email() {
    // A unique index permits many NULLs. Pinned because the obvious "fix" of
    // storing '' instead would make the second email-less user a conflict.
    let scratch = migrated_or_skip!();
    for name in ["one", "two", "three"] {
        let user = NewUser {
            username: name,
            email: None,
            ..alice()
        };
        users::create(&scratch.pool, &user)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    scratch.drop_db().await;
}

#[tokio::test]
async fn login_lookup_takes_a_username_or_an_email_and_returns_the_hash() {
    let scratch = migrated_or_skip!();
    let created = users::create(&scratch.pool, &alice()).await.unwrap();

    for login in ["alice", "ALICE@example.com"] {
        let record = users::find_for_login(&scratch.pool, login)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{login:?} found nobody"));
        assert_eq!(record.user.id, created.id);
        assert_eq!(record.password_hash.as_deref(), alice().password_hash);
    }

    scratch.drop_db().await;
}

#[tokio::test]
async fn login_lookup_does_not_confuse_one_users_name_with_anothers_email() {
    // `@` decides which column is searched, so a username that happens to
    // equal someone else's email cannot match both rows.
    let scratch = migrated_or_skip!();
    let owner = users::create(&scratch.pool, &alice()).await.unwrap();
    let squatter = NewUser {
        username: "bob",
        email: Some("bob@example.com"),
        ..alice()
    };
    users::create(&scratch.pool, &squatter).await.unwrap();

    let record = users::find_for_login(&scratch.pool, "alice@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.user.id, owner.id);

    scratch.drop_db().await;
}

#[tokio::test]
async fn the_password_hash_can_be_replaced_and_cleared() {
    let scratch = migrated_or_skip!();
    let created = users::create(&scratch.pool, &alice()).await.unwrap();

    assert!(
        users::set_password_hash(&scratch.pool, created.id, Some("new-hash"))
            .await
            .unwrap()
    );
    let record = users::find_for_login(&scratch.pool, "alice")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.password_hash.as_deref(), Some("new-hash"));

    users::set_password_hash(&scratch.pool, created.id, None)
        .await
        .unwrap();
    let record = users::find_for_login(&scratch.pool, "alice")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.password_hash, None);

    assert!(
        !users::set_password_hash(&scratch.pool, 999.into(), Some("x"))
            .await
            .unwrap(),
        "updating a missing user reported success"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn role_and_status_can_be_changed() {
    let scratch = migrated_or_skip!();
    let created = users::create(&scratch.pool, &alice()).await.unwrap();

    users::set_role(&scratch.pool, created.id, Role::Admin)
        .await
        .unwrap();
    users::set_status(&scratch.pool, created.id, UserStatus::Banned)
        .await
        .unwrap();

    let user = users::find_by_id(&scratch.pool, created.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.role, Role::Admin);
    assert_eq!(user.status, UserStatus::Banned);

    // Setting the value it already has still reports that the user exists:
    // the setters' `bool` means "found", not "changed".
    assert!(
        users::set_role(&scratch.pool, created.id, Role::Admin)
            .await
            .unwrap()
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn failed_logins_lock_the_account_at_the_threshold() {
    let scratch = migrated_or_skip!();
    let created = users::create(&scratch.pool, &alice()).await.unwrap();
    let until = now() + Duration::minutes(15);

    for attempt in 1..=2 {
        users::record_failed_login(&scratch.pool, created.id, 3, until)
            .await
            .unwrap();
        let user = users::find_by_id(&scratch.pool, created.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(user.failed_logins, attempt);
        assert_eq!(user.locked_until, None, "locked early at {attempt}");
    }

    users::record_failed_login(&scratch.pool, created.id, 3, until)
        .await
        .unwrap();
    let user = users::find_by_id(&scratch.pool, created.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.failed_logins, 3);
    assert_eq!(user.locked_until, Some(until));

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_successful_login_clears_the_lockout() {
    let scratch = migrated_or_skip!();
    let created = users::create(&scratch.pool, &alice()).await.unwrap();
    let at = now();

    users::record_failed_login(&scratch.pool, created.id, 1, at + Duration::minutes(5))
        .await
        .unwrap();
    users::record_login(&scratch.pool, created.id, at)
        .await
        .unwrap();

    let user = users::find_by_id(&scratch.pool, created.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.failed_logins, 0);
    assert_eq!(user.locked_until, None);
    assert_eq!(user.last_login_at, Some(at));

    scratch.drop_db().await;
}

#[tokio::test]
async fn concurrent_usage_updates_do_not_lose_writes() {
    // The design's counter-correctness requirement: concurrent uploads each
    // bump the owner's counters, and a read-modify-write would lose some.
    let scratch = migrated_or_skip!();
    let created = users::create(&scratch.pool, &alice()).await.unwrap();

    let tasks: Vec<_> = (0..50)
        .map(|_| {
            let pool = scratch.pool.clone();
            tokio::spawn(async move { users::adjust_usage(&pool, created.id, 1, 1000).await })
        })
        .collect();
    for task in tasks {
        task.await.expect("task panicked").expect("adjust_usage");
    }

    let user = users::find_by_id(&scratch.pool, created.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.file_count, 50);
    assert_eq!(user.disk_used, 50_000);

    scratch.drop_db().await;
}

#[tokio::test]
async fn usage_counters_never_go_below_zero() {
    // An over-eager decrement (a purge retried after a partial success) must
    // floor at zero, not fail the statement on an unsigned column.
    let scratch = migrated_or_skip!();
    let created = users::create(&scratch.pool, &alice()).await.unwrap();

    users::adjust_usage(&scratch.pool, created.id, 2, 500)
        .await
        .unwrap();
    users::adjust_usage(&scratch.pool, created.id, -5, -9_999)
        .await
        .unwrap();
    users::adjust_album_count(&scratch.pool, created.id, -1)
        .await
        .unwrap();

    let user = users::find_by_id(&scratch.pool, created.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.file_count, 0);
    assert_eq!(user.disk_used, 0);
    assert_eq!(user.album_count, 0);

    scratch.drop_db().await;
}
