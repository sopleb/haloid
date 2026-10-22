//! Authentication end to end, against a real MariaDB.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use altcha::{Challenge, Payload, SolveChallengeOptions};
use base64::Engine;
use chrono::{SubsecRound, Utc};
use haloid_core::DomainError;
use haloid_core::account::UserStatus;
use haloid_core::auth::{Actor, Role};
use haloid_service::auth::human::AltchaConfig;
use haloid_service::auth::{Auth, AuthConfig, LoginRequest, RegisterRequest};
use haloid_service::settings::SettingsHandle;
use std::collections::BTreeMap;
use std::net::IpAddr;

const PASSWORD: &str = "correct horse battery";

async fn auth(db: &haloid_db::Db) -> Auth {
    let settings = SettingsHandle::load(db.clone(), BTreeMap::new())
        .await
        .unwrap();
    let mut config = AuthConfig::new("integration-secret".into());
    // Cheap proof-of-work so tests solve in milliseconds.
    config.altcha = AltchaConfig {
        cost: 1,
        counter_min: 1,
        counter_max: 20,
        ..config.altcha
    };
    Auth::new(db.clone(), settings, config)
}

/// Ask for a challenge and solve it, as the browser widget would.
fn altcha(auth: &Auth) -> String {
    let challenge: Challenge =
        serde_json::from_value(auth.challenge(None, Utc::now()).unwrap()).unwrap();
    let solution = altcha::solve_challenge(SolveChallengeOptions::new(&challenge))
        .unwrap()
        .unwrap();
    base64::engine::general_purpose::STANDARD.encode(
        serde_json::to_vec(&Payload {
            challenge,
            solution,
        })
        .unwrap(),
    )
}

fn ip(last: u8) -> Option<IpAddr> {
    Some(IpAddr::from([192, 0, 2, last]))
}

async fn register(auth: &Auth, username: &str) -> haloid_core::account::User {
    auth.register(
        &RegisterRequest {
            username,
            email: Some(&format!("{username}@example.com")),
            password: PASSWORD,
            altcha: &altcha(auth),
            ip: ip(1),
        },
        Utc::now(),
    )
    .await
    .expect("register")
}

async fn login(
    auth: &Auth,
    login: &str,
    password: &str,
) -> Result<haloid_service::auth::LoggedIn, DomainError> {
    auth.login(
        &LoginRequest {
            login,
            password,
            altcha: &altcha(auth),
            ip: ip(2),
            user_agent: Some("test"),
        },
        Utc::now().trunc_subsecs(0),
    )
    .await
}

#[tokio::test]
async fn register_then_log_in_by_name_or_email_then_out() {
    let scratch = migrated_or_skip!();
    let auth = auth(&scratch.pool).await;
    let first = register(&auth, "founder").await;
    assert_eq!(
        first.role,
        Role::Admin,
        "the first account administers the instance"
    );
    let alice = register(&auth, "alice").await;
    assert_eq!(alice.role, Role::User, "and only the first");

    for name in ["alice", "ALICE", "alice@example.com"] {
        let logged_in = login(&auth, name, PASSWORD)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(logged_in.user.id, alice.id);
        assert!(logged_in.user.last_login_at.is_some());

        let (actor, session) = auth
            .session_actor(&logged_in.token, Utc::now())
            .await
            .unwrap()
            .expect("the cookie authenticates");
        assert_eq!(
            actor,
            Actor::User {
                id: alice.id,
                role: Role::User
            }
        );

        auth.logout(&session.id).await.unwrap();
        assert!(
            auth.session_actor(&logged_in.token, Utc::now())
                .await
                .unwrap()
                .is_none(),
            "the cookie outlived its logout"
        );
    }

    scratch.drop_db().await;
}

#[tokio::test]
async fn registration_rules_are_enforced() {
    let scratch = migrated_or_skip!();
    let auth = auth(&scratch.pool).await;
    register(&auth, "alice").await;

    let attempt = |username: &'static str, password: &'static str| {
        let field = altcha(&auth);
        let auth = &auth;
        async move {
            auth.register(
                &RegisterRequest {
                    username,
                    email: None,
                    password,
                    altcha: &field,
                    ip: ip(3),
                },
                Utc::now(),
            )
            .await
        }
    };
    assert!(matches!(
        attempt("ALICE", PASSWORD).await,
        Err(DomainError::Conflict(_))
    ));
    assert!(matches!(
        attempt("admin", PASSWORD).await,
        Err(DomainError::InvalidParam { .. })
    ));
    assert!(matches!(
        attempt("bob", "short").await,
        Err(DomainError::InvalidParam { .. })
    ));

    scratch.drop_db().await;
}

#[tokio::test]
async fn closed_registration_refuses_everyone() {
    let scratch = migrated_or_skip!();
    let settings = SettingsHandle::load(scratch.pool.clone(), BTreeMap::new())
        .await
        .unwrap();
    settings
        .update(&BTreeMap::from([(
            "site.registration_open".to_string(),
            "false".to_string(),
        )]))
        .await
        .unwrap();
    let auth = auth(&scratch.pool).await;
    let result = auth
        .register(
            &RegisterRequest {
                username: "alice",
                email: None,
                password: PASSWORD,
                altcha: &altcha(&auth),
                ip: ip(4),
            },
            Utc::now(),
        )
        .await;
    assert!(matches!(result, Err(DomainError::RegistrationClosed)));
    scratch.drop_db().await;
}

#[tokio::test]
async fn wrong_and_unknown_logins_are_indistinguishable() {
    let scratch = migrated_or_skip!();
    let auth = auth(&scratch.pool).await;
    register(&auth, "alice").await;

    assert!(matches!(
        login(&auth, "alice", "not the password").await,
        Err(DomainError::InvalidCredentials)
    ));
    assert!(matches!(
        login(&auth, "nobody", PASSWORD).await,
        Err(DomainError::InvalidCredentials)
    ));
    assert!(matches!(
        login(&auth, "nobody@example.com", PASSWORD).await,
        Err(DomainError::InvalidCredentials)
    ));

    scratch.drop_db().await;
}

#[tokio::test]
async fn repeated_failures_lock_the_account_even_against_the_right_password() {
    let scratch = migrated_or_skip!();
    let auth = auth(&scratch.pool).await;
    register(&auth, "alice").await;

    for _ in 0..5 {
        let _ = login(&auth, "alice", "guess").await;
    }
    match login(&auth, "alice", PASSWORD).await {
        Err(DomainError::AccountLocked { retry_after_secs }) => {
            assert!(retry_after_secs > 0 && retry_after_secs <= 15 * 60)
        }
        other => panic!("expected AccountLocked, got {other:?}"),
    }

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_banned_user_cannot_log_in_and_their_cookie_stops_working() {
    let scratch = migrated_or_skip!();
    let auth = auth(&scratch.pool).await;
    let alice = register(&auth, "alice").await;
    let logged_in = login(&auth, "alice", PASSWORD).await.unwrap();

    haloid_db::users::set_status(&scratch.pool, alice.id, UserStatus::Banned)
        .await
        .unwrap();
    assert!(
        auth.session_actor(&logged_in.token, Utc::now())
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        login(&auth, "alice", PASSWORD).await,
        Err(DomainError::Forbidden)
    ));

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_proof_of_work_payload_logs_in_once() {
    let scratch = migrated_or_skip!();
    let auth = auth(&scratch.pool).await;
    register(&auth, "alice").await;

    let field = altcha(&auth);
    let request = LoginRequest {
        login: "alice",
        password: PASSWORD,
        altcha: &field,
        ip: ip(5),
        user_agent: None,
    };
    auth.login(&request, Utc::now()).await.expect("first use");
    assert!(matches!(
        auth.login(&request, Utc::now()).await,
        Err(DomainError::HumanVerificationFailed)
    ));
    let mut missing = request;
    missing.altcha = "";
    assert!(matches!(
        auth.login(&missing, Utc::now()).await,
        Err(DomainError::HumanVerificationFailed)
    ));

    scratch.drop_db().await;
}

#[tokio::test]
async fn changing_the_password_logs_out_everywhere_else() {
    let scratch = migrated_or_skip!();
    let auth = auth(&scratch.pool).await;
    let alice = register(&auth, "alice").await;
    let here = login(&auth, "alice", PASSWORD).await.unwrap();
    let elsewhere = login(&auth, "alice", PASSWORD).await.unwrap();

    assert!(matches!(
        auth.change_password(alice.id, &here.session.id, "wrong", "a whole new password")
            .await,
        Err(DomainError::InvalidCredentials)
    ));
    auth.change_password(alice.id, &here.session.id, PASSWORD, "a whole new password")
        .await
        .unwrap();

    assert!(
        auth.session_actor(&here.token, Utc::now())
            .await
            .unwrap()
            .is_some(),
        "the session that made the change stays"
    );
    assert!(
        auth.session_actor(&elsewhere.token, Utc::now())
            .await
            .unwrap()
            .is_none(),
        "every other session is gone"
    );
    assert!(login(&auth, "alice", "a whole new password").await.is_ok());
    assert!(matches!(
        login(&auth, "alice", PASSWORD).await,
        Err(DomainError::InvalidCredentials)
    ));

    scratch.drop_db().await;
}

#[tokio::test]
async fn api_keys_authenticate_and_say_precisely_why_not() {
    let scratch = migrated_or_skip!();
    let auth = auth(&scratch.pool).await;
    // The first account would be the administrator; alice is a member.
    register(&auth, "founder").await;
    let alice = register(&auth, "alice").await;
    let now = Utc::now().trunc_subsecs(0);

    let (key, secret) = auth.create_api_key(Some(alice.id), "ShareX").await.unwrap();
    assert!(secret.starts_with(&key.key_prefix));
    assert_eq!(
        auth.api_key_actor(&secret, now).await.unwrap(),
        Actor::User {
            id: alice.id,
            role: Role::User
        }
    );
    let used = haloid_db::api_keys::find_by_prefix(&scratch.pool, &key.key_prefix)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(used.key.last_used_at, Some(now));

    let (_, guest_secret) = auth.create_api_key(None, "guest").await.unwrap();
    assert_eq!(
        auth.api_key_actor(&guest_secret, now).await.unwrap(),
        Actor::Guest
    );

    let tampered = format!("{}x", &secret[..secret.len() - 1]);
    for (presented, expected) in [
        ("", "missing"),
        ("   ", "missing"),
        ("not a key", "invalid"),
        (tampered.as_str(), "invalid"),
    ] {
        let error = auth.api_key_actor(presented, now).await.unwrap_err();
        let got = match error {
            DomainError::MissingApiKey => "missing",
            DomainError::InvalidApiKey => "invalid",
            other => panic!("{presented:?}: {other:?}"),
        };
        assert_eq!(got, expected, "{presented:?}");
    }

    assert!(auth.revoke_api_key(key.id, now).await.unwrap());
    assert!(matches!(
        auth.api_key_actor(&secret, now).await,
        Err(DomainError::RevokedApiKey)
    ));

    let (_, banned_key) = auth.create_api_key(Some(alice.id), "other").await.unwrap();
    haloid_db::users::set_status(&scratch.pool, alice.id, UserStatus::Banned)
        .await
        .unwrap();
    assert!(matches!(
        auth.api_key_actor(&banned_key, now).await,
        Err(DomainError::Forbidden)
    ));

    scratch.drop_db().await;
}

#[tokio::test]
async fn an_imported_bcrypt_password_works_once_and_is_then_argon2() {
    let scratch = migrated_or_skip!();
    let auth = auth(&scratch.pool).await;
    let user = register(&auth, "mira").await;
    // As the importer leaves it: PHP's password_hash("correct horse battery").
    let php = "$2y$04$IpH.kt2ZZ/Qdh3ncMlrk8O3nRU9s5cLCRd9W5OGlxJiZtHqWL/j9i";
    haloid_db::users::set_password_hash(&scratch.pool, user.id, Some(php))
        .await
        .unwrap();

    assert!(matches!(
        login(&auth, "mira", "wrong").await,
        Err(DomainError::InvalidCredentials)
    ));
    login(&auth, "mira", PASSWORD)
        .await
        .expect("the old password");
    let stored = haloid_db::users::find_for_login(&scratch.pool, "mira")
        .await
        .unwrap()
        .unwrap()
        .password_hash
        .unwrap();
    assert!(
        stored.starts_with("$argon2id$"),
        "replaced at login: {stored}"
    );
    login(&auth, "mira", PASSWORD)
        .await
        .expect("and still the same password");

    scratch.drop_db().await;
}
