//! The albums repository, against a real MariaDB.

mod common;

use chrono::{DateTime, SubsecRound, Utc};
use haloid_core::account::{User, UserStatus};
use haloid_core::auth::Role;
use haloid_core::files::{Album, AlbumPrivacy, DeleteReason, MAX_ALBUM_DEPTH};
use haloid_core::ids::{AlbumId, UserId};
use haloid_core::media::{Chain, MediaKind};
use haloid_core::pagination::Cursor;
use haloid_core::storage::StoragePathMode;
use haloid_db::albums::{self, AlbumDeletion, AlbumDetails, NewAlbum};
use haloid_db::files::{self, NewFile};
use haloid_db::users::{self, NewUser};
use haloid_db::{Db, DbError};

fn now() -> DateTime<Utc> {
    Utc::now().trunc_subsecs(0)
}

async fn user(db: &Db, name: &str) -> User {
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

fn new_album(owner: &User, parent: Option<AlbumId>) -> NewAlbum<'static> {
    NewAlbum {
        user_id: Some(owner.id),
        parent_id: parent,
        name: "Holiday",
        description: None,
        privacy: AlbumPrivacy::Public,
        password_hash: None,
    }
}

async fn create(db: &Db, owner: &User, parent: Option<AlbumId>) -> Album {
    albums::create(db, &new_album(owner, parent))
        .await
        .expect("create album")
}

/// A ready 1000-byte file in `album`.
async fn file_in(db: &Db, owner: UserId, album: AlbumId) -> haloid_core::ids::FileId {
    let new = NewFile {
        kind: MediaKind::Image,
        name: "cat-ab12cd34",
        extension: "jpg",
        original_extension: None,
        mime: "image/jpeg",
        original_filename: None,
        title: None,
        description: None,
        size: 1000,
        width: 10,
        height: 10,
        duration_ms: None,
        is_animated: false,
        frame_count: None,
        chain: Chain::from_bits(5),
        thumb_size: None,
        medium_size: None,
        frame_size: None,
        checksum: "0123456789abcdef0123456789abcdef",
        source_checksum: None,
        sha256: None,
        storage_id: None,
        storage_mode: StoragePathMode::DateFolder,
        user_id: Some(owner),
        album_id: Some(album),
        category_id: None,
        nsfw: false,
        is_approved: true,
        is_360: false,
        exif: None,
        uploader_ip: None,
        delete_token: None,
        expires_at: None,
    };
    let file = files::create_pending(db, &new, |id| format!("{id}.jpg"))
        .await
        .unwrap();
    files::mark_ready(db, file.id).await.unwrap();
    file.id
}

async fn reload(db: &Db, id: AlbumId) -> Option<Album> {
    albums::find_by_id(db, id).await.unwrap()
}

#[tokio::test]
async fn an_album_reads_back_and_is_counted_for_its_owner() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;

    let created = albums::create(
        &scratch.pool,
        &NewAlbum {
            description: Some("Beach"),
            privacy: AlbumPrivacy::Password,
            password_hash: Some("$argon2id$hash"),
            ..new_album(&alice, None)
        },
    )
    .await
    .unwrap();
    assert_eq!(created.name, "Holiday");
    assert_eq!(created.privacy, AlbumPrivacy::Password);
    assert!(created.has_password);
    assert_eq!(created.id_encoded.as_str().len(), haloid_id::PUBLIC_ID_LEN);

    assert_eq!(
        albums::find_by_id_encoded(&scratch.pool, created.id_encoded.as_str())
            .await
            .unwrap(),
        Some(created.clone())
    );
    assert_eq!(
        albums::password_hash(&scratch.pool, created.id)
            .await
            .unwrap()
            .as_deref(),
        Some("$argon2id$hash")
    );
    let alice = users::find_by_id(&scratch.pool, alice.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice.album_count, 1);

    scratch.drop_db().await;
}

#[tokio::test]
async fn details_and_cover_can_be_changed() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let created = create(&scratch.pool, &alice, None).await;
    let cover = file_in(&scratch.pool, alice.id, created.id).await;

    let details = AlbumDetails {
        name: "Renamed".into(),
        description: Some("New".into()),
        privacy: AlbumPrivacy::Unlisted,
        password_hash: None,
    };
    assert!(
        albums::update(&scratch.pool, created.id, &details)
            .await
            .unwrap()
    );
    assert!(
        albums::set_cover(&scratch.pool, created.id, Some(cover))
            .await
            .unwrap()
    );

    let album = reload(&scratch.pool, created.id).await.unwrap();
    assert_eq!(album.name, "Renamed");
    assert_eq!(album.privacy, AlbumPrivacy::Unlisted);
    assert!(!album.has_password);
    assert_eq!(album.cover_file_id, Some(cover));

    scratch.drop_db().await;
}

#[tokio::test]
async fn albums_nest_up_to_the_depth_limit_and_no_further() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;

    let mut parent = None;
    for _ in 0..MAX_ALBUM_DEPTH {
        parent = Some(create(&scratch.pool, &alice, parent).await.id);
    }
    match albums::create(&scratch.pool, &new_album(&alice, parent)).await {
        Err(DbError::Invalid { field, .. }) => assert_eq!(field, "parent_id"),
        other => panic!("nested past the limit: {other:?}"),
    }

    scratch.drop_db().await;
}

#[tokio::test]
async fn a_parent_must_exist_and_share_the_owner() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let bob = user(&scratch.pool, "bob").await;
    let bobs = create(&scratch.pool, &bob, None).await;

    for parent in [bobs.id, AlbumId(999)] {
        assert!(
            matches!(
                albums::create(&scratch.pool, &new_album(&alice, Some(parent))).await,
                Err(DbError::Invalid {
                    field: "parent_id",
                    ..
                })
            ),
            "{parent:?} accepted"
        );
    }

    scratch.drop_db().await;
}

#[tokio::test]
async fn moving_an_album_refuses_cycles_and_overdeep_trees() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let top = create(&scratch.pool, &alice, None).await;
    let middle = create(&scratch.pool, &alice, Some(top.id)).await;
    let bottom = create(&scratch.pool, &alice, Some(middle.id)).await;

    for target in [top.id, middle.id, bottom.id] {
        assert!(
            matches!(
                albums::set_parent(&scratch.pool, top.id, Some(target)).await,
                Err(DbError::Invalid { .. })
            ),
            "moved {top:?} into its own sub-tree at {target:?}"
        );
    }

    // A chain six deep has room for a two-level tree beneath it, not three.
    let mut chain = None;
    for _ in 0..6 {
        chain = Some(create(&scratch.pool, &alice, chain).await.id);
    }
    assert!(
        albums::set_parent(&scratch.pool, top.id, chain)
            .await
            .is_err()
    );
    albums::set_parent(&scratch.pool, middle.id, chain)
        .await
        .expect("a two-level sub-tree fits");
    assert_eq!(
        reload(&scratch.pool, middle.id).await.unwrap().parent_id,
        chain
    );

    albums::set_parent(&scratch.pool, middle.id, None)
        .await
        .expect("back to the top level");
    assert_eq!(
        reload(&scratch.pool, middle.id).await.unwrap().parent_id,
        None
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn deleting_and_keeping_contents_unfiles_files_and_lifts_children() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let top = create(&scratch.pool, &alice, None).await;
    let doomed = create(&scratch.pool, &alice, Some(top.id)).await;
    let child = create(&scratch.pool, &alice, Some(doomed.id)).await;
    let file = file_in(&scratch.pool, alice.id, doomed.id).await;

    assert_eq!(
        albums::delete(&scratch.pool, doomed.id, AlbumDeletion::KeepContents)
            .await
            .unwrap(),
        Some(vec![]),
        "keeping contents deletes no file"
    );
    assert!(reload(&scratch.pool, doomed.id).await.is_none());

    let file = files::find_by_id(&scratch.pool, file)
        .await
        .unwrap()
        .unwrap();
    assert!(file.is_live(now()), "the file was deleted with its album");
    assert_eq!((file.album_id, file.album_position), (None, None));
    assert_eq!(
        reload(&scratch.pool, child.id).await.unwrap().parent_id,
        Some(top.id),
        "the sub-album moves up to the deleted album's parent"
    );

    let alice = users::find_by_id(&scratch.pool, alice.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice.album_count, 2);
    assert_eq!(
        alice.file_count, 1,
        "keeping contents keeps the file counted"
    );

    assert!(
        albums::delete(&scratch.pool, doomed.id, AlbumDeletion::KeepContents)
            .await
            .unwrap()
            .is_none()
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn deleting_with_contents_takes_the_subtree_and_retains_its_files() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let survivor = create(&scratch.pool, &alice, None).await;
    let doomed = create(&scratch.pool, &alice, None).await;
    let child = create(&scratch.pool, &alice, Some(doomed.id)).await;
    let grandchild = create(&scratch.pool, &alice, Some(child.id)).await;
    let kept = file_in(&scratch.pool, alice.id, survivor.id).await;
    let in_doomed = file_in(&scratch.pool, alice.id, doomed.id).await;
    let in_grandchild = file_in(&scratch.pool, alice.id, grandchild.id).await;

    let at = now();
    let mut deleted = albums::delete(
        &scratch.pool,
        doomed.id,
        AlbumDeletion::WithContents {
            by: Some(alice.id),
            at,
        },
    )
    .await
    .unwrap()
    .unwrap();
    deleted.sort();
    let mut expected = vec![in_doomed, in_grandchild];
    expected.sort();
    assert_eq!(deleted, expected, "the caller learns which files went");

    for album in [doomed.id, child.id, grandchild.id] {
        assert!(
            reload(&scratch.pool, album).await.is_none(),
            "{album:?} survived"
        );
    }
    assert!(reload(&scratch.pool, survivor.id).await.is_some());

    for id in [in_doomed, in_grandchild] {
        let file = files::find_by_id(&scratch.pool, id)
            .await
            .unwrap()
            .expect("deleted with the album, but retained");
        let deletion = file.deletion.expect("recorded as deleted");
        assert_eq!(deletion.reason, DeleteReason::Album);
        assert_eq!(deletion.by, Some(alice.id));
        assert_eq!(deletion.at, at);
    }
    assert!(
        files::find_by_id(&scratch.pool, kept)
            .await
            .unwrap()
            .unwrap()
            .is_live(now())
    );

    let alice = users::find_by_id(&scratch.pool, alice.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice.album_count, 1);
    assert_eq!((alice.file_count, alice.disk_used), (1, 1000));

    scratch.drop_db().await;
}

#[tokio::test]
async fn listing_pages_through_one_level_newest_first() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let parent = create(&scratch.pool, &alice, None).await;
    let mut children = Vec::new();
    for _ in 0..5 {
        children.push(create(&scratch.pool, &alice, Some(parent.id)).await.id);
    }
    albums::update(
        &scratch.pool,
        children[0],
        &AlbumDetails {
            name: "Secret".into(),
            description: None,
            privacy: AlbumPrivacy::Private,
            password_hash: None,
        },
    )
    .await
    .unwrap();

    let roots = albums::list(&scratch.pool, alice.id, None, false, None, 10)
        .await
        .unwrap();
    assert_eq!(
        roots.items.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![parent.id]
    );

    let mut seen = Vec::new();
    let mut after: Option<Cursor<_>> = None;
    loop {
        let page = albums::list(&scratch.pool, alice.id, Some(parent.id), false, after, 2)
            .await
            .unwrap();
        seen.extend(page.items.iter().map(|a| a.id));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    children.reverse();
    assert_eq!(seen, children);

    let listed = albums::list(&scratch.pool, alice.id, Some(parent.id), true, None, 10)
        .await
        .unwrap();
    assert_eq!(listed.items.len(), 4, "the private album was listed");

    scratch.drop_db().await;
}
