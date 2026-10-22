//! The optional online licence check, against a stand-in licence service.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use chrono::Utc;
use haloid_core::entitlements::LicenceState;
use haloid_service::licensing::{Licensing, OnlineStatus};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SIGNER: [u8; 32] = [42; 32];

#[tokio::test]
async fn only_a_definite_answer_from_the_service_changes_anything() {
    let scratch = migrated_or_skip!();
    let db = &scratch.pool;
    haloid_db::instance::ensure(db, "inst_online")
        .await
        .unwrap();
    let key = haloid_licence::sign(
        &haloid_licence::Licence {
            id: "lic_online".into(),
            instance: "inst_online".into(),
            licensee: "Example".into(),
            kind: haloid_licence::Kind::Standard,
            issued_at: Utc::now(),
            expires_at: Some(Utc::now() + chrono::Duration::days(700)),
        },
        &SIGNER,
    );
    let licensing = Licensing::load(
        db.clone(),
        vec![haloid_licence::public_key(&SIGNER)],
        Some(key),
    )
    .await
    .unwrap();
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();
    let state = || licensing.entitlements(Utc::now());

    // Down: the offline check stands.
    let gone = MockServer::start().await;
    let address = gone.uri();
    drop(gone);
    assert!(licensing.check_online(&http, &address).await.is_err());
    assert_eq!(state().state, LicenceState::Active);

    let service = MockServer::start().await;
    let answer = |status: &str| {
        Mock::given(method("POST"))
            .and(path("/api/v1/check"))
            .and(body_partial_json(
                serde_json::json!({"licence": "lic_online", "instance": "inst_online"}),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": status})),
            )
    };

    // Revoked: branding back, no new Pro use.
    answer("revoked").up_to_n_times(1).mount(&service).await;
    assert_eq!(
        licensing.check_online(&http, &service.uri()).await,
        Ok(Some(OnlineStatus::Revoked))
    );
    assert_eq!(state().state, LicenceState::Revoked);
    assert!(state().branding && !state().pro);

    // An unknown key or a server error leaves that as it is...
    answer("unknown").up_to_n_times(1).mount(&service).await;
    assert_eq!(
        licensing.check_online(&http, &service.uri()).await,
        Ok(Some(OnlineStatus::Unknown))
    );
    assert_eq!(state().state, LicenceState::Revoked);

    // ...and active again restores it, say after a mistaken revocation.
    answer("active").mount(&service).await;
    assert_eq!(
        licensing.check_online(&http, &service.uri()).await,
        Ok(Some(OnlineStatus::Active))
    );
    assert_eq!(state().state, LicenceState::Active);

    scratch.drop_db().await;
}
