//! The updater: finding a release, installing it, and refusing anything
//! that does not check out. Against a real MariaDB (settings, audit) and a
//! stand-in for the release host.

#[path = "../../haloid-db/tests/common/mod.rs"]
mod common;

use chrono::{TimeZone, Timelike, Utc};
use haloid_core::auth::{Actor, Role};
use haloid_core::ids::UserId;
use haloid_release::{Edition, Manifest, public_key, sign};
use haloid_service::admin::By;
use haloid_service::settings::SettingsHandle;
use haloid_service::updates::{Installer, Phase, Source, Updates, UpdatesConfig};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SIGNER: [u8; 32] = [5; 32];
const TARGET: &str = "x86_64-unknown-linux-musl";

fn admin() -> By {
    By {
        actor: Actor::User {
            id: UserId(1),
            role: Role::Admin,
        },
        ip: None,
    }
}

/// An administrator who exists, for the audit log.
async fn real_admin(db: &haloid_db::Db) -> By {
    let user = haloid_db::users::create(
        db,
        &haloid_db::users::NewUser {
            username: "root",
            email: None,
            password_hash: None,
            role: Role::Admin,
            status: haloid_core::account::UserStatus::Valid,
        },
    )
    .await
    .unwrap();
    By {
        actor: Actor::User {
            id: user.id,
            role: Role::Admin,
        },
        ip: None,
    }
}

/// A stand-in binary: a script that answers `--version` like the server.
fn script(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho \"haloid-server {version} community {TARGET}\"\n").into_bytes()
}

fn install_dir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("haloid-server");
    std::fs::write(&exe, script("0.1.0")).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, exe)
}

/// A GitLab-shaped release host offering `binary` as `version`, its
/// manifest signed by `signer`.
async fn host(version: &str, binary: &[u8], advertised: &[u8], signer: [u8; 32]) -> MockServer {
    let server = MockServer::start().await;
    let manifest = Manifest {
        version: version.into(),
        edition: Edition::Community,
        target: TARGET.into(),
        size: advertised.len() as u64,
        sha256: hex::encode(Sha256::digest(advertised)),
        url: format!("{}/downloads/haloid-server", server.uri()),
        notes_url: Some(format!("{}/releases/v{version}", server.uri())),
        published_at: Utc.with_ymd_and_hms(2026, 11, 1, 9, 0, 0).unwrap(),
    };
    let name = format!("haloid-server-community-{TARGET}.manifest");
    Mock::given(method("GET"))
        .and(path("/api/releases/permalink/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tag_name": format!("v{version}"),
            "assets": {"links": [
                {"name": "haloid-server-community-aarch64-unknown-linux-musl.manifest",
                 "direct_asset_url": format!("{}/downloads/other.manifest", server.uri())},
                {"name": name, "direct_asset_url": format!("{}/downloads/{name}", server.uri())},
            ]}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/downloads/{name}")))
        .respond_with(ResponseTemplate::new(200).set_body_string(sign(&manifest, &signer)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/downloads/haloid-server"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(binary.to_vec()))
        .mount(&server)
        .await;
    server
}

async fn updates(db: &haloid_db::Db, server: &MockServer, exe: Option<&Path>) -> Arc<Updates> {
    let settings = SettingsHandle::load(db.clone(), BTreeMap::new())
        .await
        .unwrap();
    Updates::new(
        db.clone(),
        settings,
        None,
        UpdatesConfig {
            edition: Edition::Community,
            target: TARGET.into(),
            current: "0.1.0".into(),
            installer: Installer::Binary {
                exe: exe.map(Path::to_path_buf),
            },
            source: Source::Gitlab {
                api: format!("{}/api", server.uri()),
            },
            trusted: vec![public_key(&SIGNER)],
            disabled: None,
        },
    )
}

async fn wait_for_phase(updates: &Updates, done: impl Fn(&Phase) -> bool) -> Phase {
    for _ in 0..200 {
        let phase = updates.status(admin().actor).unwrap().phase;
        if done(&phase) {
            return phase;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("the install did not finish");
}

#[tokio::test]
async fn a_newer_signed_release_is_found_installed_and_the_server_told_to_restart() {
    let scratch = migrated_or_skip!();
    let (_dir, exe) = install_dir();
    let new = script("0.2.0");
    let server = host("0.2.0", &new, &new, SIGNER).await;
    let updates = updates(&scratch.pool, &server, Some(&exe)).await;
    let mut restart = updates.restart_requested();

    // Nothing known until it looks.
    assert!(updates.status(admin().actor).unwrap().available.is_none());
    updates.check().await.unwrap();
    let status = updates.status(admin().actor).unwrap();
    let available = status.available.expect("0.2.0 is offered");
    assert_eq!(available.version, "0.2.0");
    assert!(status.installable.is_ok(), "{:?}", status.installable);

    // Only administrators.
    let member = Actor::User {
        id: UserId(2),
        role: Role::User,
    };
    assert!(updates.status(member).is_err());

    updates
        .install(real_admin(&scratch.pool).await)
        .await
        .unwrap();
    assert_eq!(
        wait_for_phase(&updates, |p| matches!(
            p,
            Phase::Restarting | Phase::Failed(_)
        ))
        .await,
        Phase::Restarting
    );
    assert_eq!(std::fs::read(&exe).unwrap(), new);
    assert_eq!(
        std::fs::read(exe.with_file_name("haloid-server.previous")).unwrap(),
        script("0.1.0")
    );
    let mode = std::fs::metadata(&exe).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111, "the new binary is executable");
    restart.changed().await.unwrap();
    assert!(*restart.borrow());

    let action: String = sqlx::query_scalar(
        "SELECT action FROM ih_audit_log WHERE action LIKE 'update.%' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&scratch.pool)
    .await
    .unwrap();
    assert_eq!(action, "update.install");
    scratch.drop_db().await;
}

#[tokio::test]
async fn a_release_signed_by_someone_else_is_never_offered() {
    let scratch = migrated_or_skip!();
    let (_dir, exe) = install_dir();
    let server = host("0.2.0", &script("0.2.0"), &script("0.2.0"), [6; 32]).await;
    let updates = updates(&scratch.pool, &server, Some(&exe)).await;
    assert!(updates.check().await.is_err());
    let status = updates.status(admin().actor).unwrap();
    assert!(status.available.is_none());
    assert!(status.error.unwrap().contains("signature"));
    assert!(updates.install(admin()).await.is_err());
    scratch.drop_db().await;
}

#[tokio::test]
async fn a_download_that_differs_from_its_manifest_changes_nothing() {
    let scratch = migrated_or_skip!();
    let (dir, exe) = install_dir();
    // The manifest describes one file; the host serves another.
    let server = host("0.2.0", &script("6.6.6"), &script("0.2.0"), SIGNER).await;
    let updates = updates(&scratch.pool, &server, Some(&exe)).await;
    updates.check().await.unwrap();
    updates
        .install(real_admin(&scratch.pool).await)
        .await
        .unwrap();
    let phase = wait_for_phase(&updates, |p| {
        matches!(p, Phase::Restarting | Phase::Failed(_))
    })
    .await;
    assert!(matches!(phase, Phase::Failed(_)), "{phase:?}");
    assert_eq!(std::fs::read(&exe).unwrap(), script("0.1.0"));
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(left.len(), 1, "no download left behind: {left:?}");
    scratch.drop_db().await;
}

#[tokio::test]
async fn a_binary_that_does_not_say_the_promised_version_is_not_swapped_in() {
    let scratch = migrated_or_skip!();
    let (_dir, exe) = install_dir();
    // Signed and hashed correctly, but it is not 0.2.0 when run.
    let wrong = script("0.1.5");
    let server = host("0.2.0", &wrong, &wrong, SIGNER).await;
    let updates = updates(&scratch.pool, &server, Some(&exe)).await;
    updates.check().await.unwrap();
    updates
        .install(real_admin(&scratch.pool).await)
        .await
        .unwrap();
    let phase = wait_for_phase(&updates, |p| {
        matches!(p, Phase::Restarting | Phase::Failed(_))
    })
    .await;
    assert!(
        matches!(phase, Phase::Failed(ref m) if m.contains("version")),
        "{phase:?}"
    );
    assert_eq!(std::fs::read(&exe).unwrap(), script("0.1.0"));
    scratch.drop_db().await;
}

#[tokio::test]
async fn without_a_writable_install_it_only_tells() {
    let scratch = migrated_or_skip!();
    let new = script("0.2.0");
    let server = host("0.2.0", &new, &new, SIGNER).await;
    // No known binary path at all.
    let updates = updates(&scratch.pool, &server, None).await;
    updates.check().await.unwrap();
    let status = updates.status(admin().actor).unwrap();
    assert!(status.available.is_some());
    assert!(status.installable.is_err());
    assert!(updates.install(admin()).await.is_err());

    // A directory the server may not write to.
    let (dir, exe) = install_dir();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let updates = self::updates(&scratch.pool, &server, Some(&exe)).await;
    let status = updates.status(admin().actor).unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    // Root can write anywhere; the check is only meaningful otherwise.
    if !is_root() {
        assert!(status.installable.unwrap_err().contains("writable"));
    }
    scratch.drop_db().await;
}

#[tokio::test]
async fn in_a_container_there_is_no_updater() {
    let scratch = migrated_or_skip!();
    let (_dir, exe) = install_dir();
    let server = host("0.2.0", &script("0.2.0"), &script("0.2.0"), SIGNER).await;
    let settings = SettingsHandle::load(scratch.pool.clone(), BTreeMap::new())
        .await
        .unwrap();
    let updates = Updates::new(
        scratch.pool.clone(),
        settings,
        None,
        UpdatesConfig {
            edition: Edition::Community,
            target: TARGET.into(),
            current: "0.1.0".into(),
            installer: Installer::Binary { exe: Some(exe) },
            source: Source::Gitlab {
                api: format!("{}/api", server.uri()),
            },
            trusted: vec![public_key(&SIGNER)],
            disabled: Some("running in a container".into()),
        },
    );
    assert!(updates.check().await.is_err());
    let status = updates.status(admin().actor).unwrap();
    assert!(!status.enabled);
    assert!(status.available.is_none());
    assert!(server.received_requests().await.unwrap().is_empty());
    scratch.drop_db().await;
}

#[tokio::test]
async fn auto_mode_installs_at_its_hour_and_notify_mode_never_does() {
    let scratch = migrated_or_skip!();
    let (_dir, exe) = install_dir();
    let new = script("0.2.0");
    let server = host("0.2.0", &new, &new, SIGNER).await;
    let updates = updates(&scratch.pool, &server, Some(&exe)).await;
    let at = |hour| Utc.with_ymd_and_hms(2026, 11, 2, hour, 10, 0).unwrap();

    // Notify (the default): it looks, and leaves the install to a person.
    updates.tick(at(4)).await;
    assert!(updates.status(admin().actor).unwrap().available.is_some());
    assert_eq!(updates.status(admin().actor).unwrap().phase, Phase::Idle);

    sqlx::query("INSERT INTO ih_settings (name, value) VALUES ('updates.mode', 'auto')")
        .execute(&scratch.pool)
        .await
        .unwrap();
    updates.settings().reload().await.unwrap();
    updates.tick(at(3)).await;
    assert_eq!(
        updates.status(admin().actor).unwrap().phase,
        Phase::Idle,
        "not its hour"
    );
    let at_four = at(4);
    assert_eq!(at_four.hour(), 4);
    updates.tick(at_four).await;
    assert_eq!(
        wait_for_phase(&updates, |p| matches!(
            p,
            Phase::Restarting | Phase::Failed(_)
        ))
        .await,
        Phase::Restarting
    );
    assert_eq!(std::fs::read(&exe).unwrap(), new);
    scratch.drop_db().await;
}

fn is_root() -> bool {
    std::fs::metadata("/proc/self")
        .map(|m| std::os::unix::fs::MetadataExt::uid(&m) == 0)
        .unwrap_or(false)
}

/// A GitLab-shaped host offering an image release, and a stand-in sidecar
/// that accepts `status` for an update.
async fn image_host(version: &str, sidecar_status: u16) -> (MockServer, String) {
    use wiremock::matchers::header;
    let server = MockServer::start().await;
    let release = haloid_release::ImageRelease {
        version: version.into(),
        edition: Edition::Community,
        image: format!("registry.example.com/haloid@sha256:{}", "d1".repeat(32)),
        notes_url: None,
        published_at: Utc.with_ymd_and_hms(2026, 11, 1, 9, 0, 0).unwrap(),
    };
    let text = haloid_release::sign_image(&release, &SIGNER);
    Mock::given(method("GET"))
        .and(path("/api/releases/permalink/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "assets": {"links": [
                {"name": format!("haloid-server-community-{TARGET}.manifest"),
                 "direct_asset_url": format!("{}/downloads/binary.manifest", server.uri())},
                {"name": "haloid-image-community.manifest",
                 "direct_asset_url": format!("{}/downloads/image.manifest", server.uri())},
            ]}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/downloads/image.manifest"))
        .respond_with(ResponseTemplate::new(200).set_body_string(text.clone()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sidecar/v1/update"))
        .and(header("authorization", "Bearer sidecar-token"))
        .and(wiremock::matchers::body_string_contains(text.as_str()))
        .respond_with(
            ResponseTemplate::new(sidecar_status)
                .set_body_json(serde_json::json!({"error": "the container runs 9.9.9: not newer"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sidecar/v1/status"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"phase": "idle", "version": null, "detail": null}),
            ),
        )
        .mount(&server)
        .await;
    (server, text)
}

async fn in_a_container(
    db: &haloid_db::Db,
    server: &MockServer,
    installer: Installer,
) -> Arc<Updates> {
    let settings = SettingsHandle::load(db.clone(), BTreeMap::new())
        .await
        .unwrap();
    Updates::new(
        db.clone(),
        settings,
        None,
        UpdatesConfig {
            edition: Edition::Community,
            target: TARGET.into(),
            current: "0.1.0".into(),
            installer,
            source: Source::Gitlab {
                api: format!("{}/api", server.uri()),
            },
            trusted: vec![public_key(&SIGNER)],
            disabled: None,
        },
    )
}

#[tokio::test]
async fn in_a_container_the_image_release_goes_to_the_sidecar() {
    let scratch = migrated_or_skip!();
    let (server, _) = image_host("0.2.0", 202).await;
    let sidecar = Installer::Sidecar {
        url: format!("{}/sidecar", server.uri()),
        token: "sidecar-token".into(),
    };
    let updates = in_a_container(&scratch.pool, &server, sidecar).await;
    updates.check().await.unwrap();
    let status = updates.status(admin().actor).unwrap();
    assert_eq!(status.available.unwrap().version, "0.2.0");
    assert!(status.installable.is_ok());
    updates
        .install(real_admin(&scratch.pool).await)
        .await
        .unwrap();
    assert_eq!(
        updates.status(admin().actor).unwrap().phase,
        Phase::Restarting,
        "the sidecar took it, and will stop this container"
    );
    scratch.drop_db().await;
}

#[tokio::test]
async fn a_sidecar_that_refuses_leaves_the_reason_on_the_page() {
    let scratch = migrated_or_skip!();
    let (server, _) = image_host("0.2.0", 422).await;
    let sidecar = Installer::Sidecar {
        url: format!("{}/sidecar", server.uri()),
        token: "sidecar-token".into(),
    };
    let updates = in_a_container(&scratch.pool, &server, sidecar).await;
    updates.check().await.unwrap();
    assert!(
        updates
            .install(real_admin(&scratch.pool).await)
            .await
            .is_err()
    );
    let phase = updates.status(admin().actor).unwrap().phase;
    assert!(
        matches!(phase, Phase::Failed(ref m) if m.contains("not newer")),
        "{phase:?}"
    );
    scratch.drop_db().await;
}

#[tokio::test]
async fn a_container_without_the_sidecar_shows_the_release_and_how_to_pull_it() {
    let scratch = migrated_or_skip!();
    let (server, _) = image_host("0.2.0", 202).await;
    let updates = in_a_container(&scratch.pool, &server, Installer::Container).await;
    updates.check().await.unwrap();
    let status = updates.status(admin().actor).unwrap();
    assert_eq!(status.available.unwrap().version, "0.2.0");
    assert!(
        status
            .installable
            .unwrap_err()
            .contains("docker compose pull")
    );
    assert!(
        updates
            .install(real_admin(&scratch.pool).await)
            .await
            .is_err()
    );
    scratch.drop_db().await;
}
