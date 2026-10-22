//! The operator's way back in, against a real MariaDB.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use chrono::{Duration, Utc};
use haloid_core::DomainError;
use haloid_core::account::UserStatus;
use haloid_core::auth::Role;
use haloid_db::{audit, users};
use haloid_service::rescue;

#[tokio::test]
async fn an_operator_promotes_a_user_and_resets_a_password_and_both_are_audited() {
    let scratch = migrated_or_skip!();
    let db = &scratch.pool;
    let create = |name: &'static str, role, status| {
        let email = format!("{name}@example.com");
        async move {
            users::create(
                db,
                &users::NewUser {
                    username: name,
                    email: Some(&email),
                    password_hash: None,
                    role,
                    status,
                },
            )
            .await
            .unwrap()
        }
    };
    let banned = create("mira", Role::User, UserStatus::Banned).await;
    create("tomas", Role::User, UserStatus::Valid).await;
    assert!(rescue::admins(db).await.unwrap().is_empty());

    // By email as well as username; a ban and a lockout are lifted.
    users::record_failed_login(db, banned.id, 1, Utc::now() + Duration::hours(1))
        .await
        .unwrap();
    let promoted = rescue::promote(db, "MIRA@example.com").await.unwrap();
    assert_eq!(promoted.role, Role::Admin);
    assert_eq!(promoted.status, UserStatus::Valid);
    let login = users::find_for_login(db, "mira").await.unwrap().unwrap();
    assert_eq!(login.user.locked_until, None);
    let admins = rescue::admins(db).await.unwrap();
    assert_eq!(
        admins
            .iter()
            .map(|u| u.username.as_str())
            .collect::<Vec<_>>(),
        ["mira"]
    );

    // A password is held to the usual rules, and set.
    assert!(matches!(
        rescue::set_password(db, "mira", "short").await,
        Err(DomainError::InvalidParam { .. })
    ));
    rescue::set_password(db, "mira", "correct horse battery")
        .await
        .unwrap();
    let hash = users::find_for_login(db, "mira")
        .await
        .unwrap()
        .unwrap()
        .password_hash
        .unwrap();
    assert!(haloid_service::auth::secrets::verify_password(
        "correct horse battery",
        &hash
    ));

    assert!(matches!(
        rescue::promote(db, "nobody").await,
        Err(DomainError::NotFound("user"))
    ));
    let log = audit::list(db, None, 10).await.unwrap();
    let actions: Vec<&str> = log.iter().map(|e| e.action.as_str()).collect();
    assert!(
        actions.contains(&"user.promote") && actions.contains(&"user.password_reset"),
        "{actions:?}"
    );
    assert!(log.iter().all(|e| e.actor.is_none()));

    scratch.drop_db().await;
}
