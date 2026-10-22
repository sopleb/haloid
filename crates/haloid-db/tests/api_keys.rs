//! The API keys repository, against a real MariaDB.

mod common;

use chrono::{DateTime, SubsecRound, Utc};
use haloid_core::account::{User, UserStatus};
use haloid_core::auth::Role;
use haloid_db::DbError;
use haloid_db::api_keys::{self, NewApiKey};
use haloid_db::users::{self, NewUser};

/// Any well-formed hex digest; nothing here verifies it.
const SECRET_HASH: &str = "abababababababababababababababababababababababababababababababab";

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

fn key<'a>(user: Option<&User>, prefix: &'a str) -> NewApiKey<'a> {
    NewApiKey {
        user_id: user.map(|u| u.id),
        label: "laptop",
        key_prefix: prefix,
        secret_hash: SECRET_HASH,
        hmac_key: &[7; 32],
    }
}

#[tokio::test]
async fn a_key_is_found_by_its_prefix_with_its_secret_material() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;

    let created = api_keys::create(&scratch.pool, &key(Some(&alice), "hld_aaaaaaaaaa_"))
        .await
        .expect("create key");
    assert_eq!(created.user_id, Some(alice.id));
    assert_eq!(created.label, "laptop");
    assert_eq!(created.key_prefix, "hld_aaaaaaaaaa_");
    assert!(created.is_active);
    assert!(!created.is_guest_key());
    assert_eq!(created.last_used_at, None);
    assert_eq!(created.revoked_at, None);

    let record = api_keys::find_by_prefix(&scratch.pool, "hld_aaaaaaaaaa_")
        .await
        .unwrap()
        .expect("found");
    assert_eq!(record.key, created);
    assert_eq!(record.secret_hash, SECRET_HASH);
    assert_eq!(record.hmac_key, vec![7; 32]);

    assert!(
        api_keys::find_by_prefix(&scratch.pool, "hld_bbbbbbbbbb_")
            .await
            .unwrap()
            .is_none()
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_duplicate_prefix_is_a_conflict() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    api_keys::create(&scratch.pool, &key(Some(&alice), "hld_aaaaaaaaaa_"))
        .await
        .unwrap();

    match api_keys::create(&scratch.pool, &key(Some(&alice), "hld_aaaaaaaaaa_")).await {
        Err(DbError::Conflict(field)) => assert_eq!(field, "key_prefix"),
        other => panic!("expected a key_prefix conflict, got {other:?}"),
    }

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_revoked_key_is_still_found_so_it_can_be_reported_as_revoked() {
    // The legacy surface distinguishes "invalid key" from "revoked key", so
    // the lookup must not hide revoked rows.
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let created = api_keys::create(&scratch.pool, &key(Some(&alice), "hld_aaaaaaaaaa_"))
        .await
        .unwrap();
    let at = now();

    assert!(
        api_keys::revoke(&scratch.pool, created.id, at)
            .await
            .unwrap()
    );
    assert!(
        !api_keys::revoke(&scratch.pool, created.id, at)
            .await
            .unwrap(),
        "revoking twice reported a change"
    );

    let record = api_keys::find_by_prefix(&scratch.pool, "hld_aaaaaaaaaa_")
        .await
        .unwrap()
        .expect("a revoked key is still found");
    assert!(!record.key.is_active);
    assert_eq!(record.key.revoked_at, Some(at));

    scratch.drop_db().await;
}

#[tokio::test]
async fn listing_separates_users_from_guest_keys() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let bob = user(&scratch.pool, "bob").await;

    api_keys::create(&scratch.pool, &key(Some(&alice), "hld_alice00001_"))
        .await
        .unwrap();
    api_keys::create(&scratch.pool, &key(Some(&alice), "hld_alice00002_"))
        .await
        .unwrap();
    api_keys::create(&scratch.pool, &key(Some(&bob), "hld_bob0000001_"))
        .await
        .unwrap();
    let guest = api_keys::create(&scratch.pool, &key(None, "hld_guest00001_"))
        .await
        .unwrap();
    assert!(guest.is_guest_key());

    let prefixes = |keys: Vec<haloid_core::account::ApiKey>| {
        keys.into_iter().map(|k| k.key_prefix).collect::<Vec<_>>()
    };
    assert_eq!(
        prefixes(api_keys::list(&scratch.pool, Some(alice.id)).await.unwrap()),
        vec!["hld_alice00001_", "hld_alice00002_"]
    );
    assert_eq!(
        prefixes(api_keys::list(&scratch.pool, None).await.unwrap()),
        vec!["hld_guest00001_"],
        "guest keys are the ones with no owner, and only those"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn using_a_key_records_when() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let created = api_keys::create(&scratch.pool, &key(Some(&alice), "hld_aaaaaaaaaa_"))
        .await
        .unwrap();
    let at = now();

    api_keys::touch(&scratch.pool, created.id, at)
        .await
        .unwrap();
    let record = api_keys::find_by_prefix(&scratch.pool, "hld_aaaaaaaaaa_")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.key.last_used_at, Some(at));

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_users_keys_go_with_the_user() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    api_keys::create(&scratch.pool, &key(Some(&alice), "hld_aaaaaaaaaa_"))
        .await
        .unwrap();

    sqlx::query("DELETE FROM ih_users WHERE id = ?")
        .bind(alice.id.0)
        .execute(&scratch.pool)
        .await
        .unwrap();

    assert!(
        api_keys::find_by_prefix(&scratch.pool, "hld_aaaaaaaaaa_")
            .await
            .unwrap()
            .is_none(),
        "a deleted user's key still authenticates"
    );

    scratch.drop_db().await;
}
