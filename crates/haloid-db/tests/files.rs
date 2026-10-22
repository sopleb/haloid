//! The files repository, against a real MariaDB.

mod common;

use chrono::{DateTime, Duration, SubsecRound, Utc};
use haloid_core::account::{User, UserStatus};
use haloid_core::auth::Role;
use haloid_core::files::{AlbumPrivacy, DeleteReason, Deletion, FileState, MediaFile};
use haloid_core::ids::{AlbumId, CategoryId, FileId, UserId};
use haloid_core::media::{Chain, MediaKind};
use haloid_core::pagination::{Cursor, FileSort};
use haloid_core::storage::StoragePathMode;
use haloid_db::albums::{self, NewAlbum};
use haloid_db::files::{self, FileDetails, FileQuery, FileScope, NewFile, Visibility};
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

async fn album(db: &Db, owner: &User, privacy: AlbumPrivacy) -> AlbumId {
    albums::create(
        db,
        &NewAlbum {
            user_id: Some(owner.id),
            parent_id: None,
            name: "Holiday",
            description: None,
            privacy,
            password_hash: None,
        },
    )
    .await
    .expect("create album")
    .id
}

async fn category(db: &Db, key: &str) -> CategoryId {
    let done = sqlx::query("INSERT INTO ih_categories (name, url_key) VALUES (?, ?)")
        .bind(key)
        .bind(key)
        .execute(db)
        .await
        .expect("create category");
    CategoryId(u32::try_from(done.last_insert_id()).unwrap())
}

/// A 1000-byte image with a 100-byte thumb: 1100 stored bytes.
fn image(owner: Option<UserId>) -> NewFile<'static> {
    NewFile {
        kind: MediaKind::Image,
        name: "cat-ab12cd34",
        extension: "jpg",
        original_extension: None,
        mime: "image/jpeg",
        original_filename: Some("cat.jpg"),
        title: None,
        description: None,
        size: 1000,
        width: 640,
        height: 480,
        duration_ms: None,
        is_animated: false,
        frame_count: None,
        chain: Chain::from_bits(7),
        thumb_size: Some(100),
        medium_size: None,
        frame_size: None,
        checksum: "0123456789abcdef0123456789abcdef",
        source_checksum: Some("0123456789abcdef0123456789abcdef"),
        sha256: None,
        storage_id: None,
        storage_mode: StoragePathMode::DateFolder,
        user_id: owner,
        album_id: None,
        category_id: None,
        nsfw: false,
        is_approved: true,
        is_360: false,
        exif: None,
        uploader_ip: None,
        delete_token: None,
        expires_at: None,
    }
}

fn path(id: &str) -> String {
    format!("2026/10/21/{id}.jpg")
}

/// Create and publish.
async fn ready(db: &Db, new: &NewFile<'_>) -> MediaFile {
    let file = files::create_pending(db, new, path).await.expect("create");
    assert!(files::mark_ready(db, file.id).await.expect("mark_ready"));
    files::find_by_id(db, file.id).await.unwrap().unwrap()
}

async fn reload_user(db: &Db, id: UserId) -> User {
    users::find_by_id(db, id).await.unwrap().unwrap()
}

fn deletion(by: Option<UserId>, reason: DeleteReason) -> Deletion {
    Deletion {
        at: now(),
        by,
        reason,
    }
}

fn query(scope: FileScope, visibility: Visibility, sort: FileSort) -> FileQuery {
    FileQuery {
        scope,
        visibility,
        include_nsfw: true,
        kind: None,
        sort,
        after: None,
        limit: 50,
        now: now(),
    }
}

async fn ids(db: &Db, q: &FileQuery) -> Vec<FileId> {
    files::list(db, q)
        .await
        .expect("list")
        .items
        .into_iter()
        .map(|f| f.id)
        .collect()
}

#[tokio::test]
async fn a_pending_file_is_invisible_and_uncounted_until_ready() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let holiday = album(&scratch.pool, &alice, AlbumPrivacy::Public).await;
    let art = category(&scratch.pool, "art").await;

    let pending = files::create_pending(
        &scratch.pool,
        &NewFile {
            album_id: Some(holiday),
            category_id: Some(art),
            ..image(Some(alice.id))
        },
        path,
    )
    .await
    .expect("create");
    assert_eq!(pending.state, FileState::Pending);
    assert_eq!(pending.id_encoded.as_str().len(), haloid_id::PUBLIC_ID_LEN);
    assert_eq!(
        pending.storage_path,
        path(pending.id_encoded.as_str()),
        "the storage path is built from the drawn public id"
    );
    assert_eq!(pending.album_position, Some(1));

    let owner_view = query(
        FileScope::User(alice.id),
        Visibility::Owner,
        FileSort::Newest,
    );
    assert!(
        ids(&scratch.pool, &owner_view).await.is_empty(),
        "pending was listed"
    );
    assert_eq!(reload_user(&scratch.pool, alice.id).await.file_count, 0);

    assert!(files::mark_ready(&scratch.pool, pending.id).await.unwrap());
    assert!(
        !files::mark_ready(&scratch.pool, pending.id).await.unwrap(),
        "publishing twice reported success"
    );

    assert_eq!(ids(&scratch.pool, &owner_view).await, vec![pending.id]);
    let alice = reload_user(&scratch.pool, alice.id).await;
    assert_eq!(alice.file_count, 1, "counted once, not twice");
    assert_eq!(
        alice.disk_used, 1100,
        "every stored byte, derivatives included"
    );
    let holiday = albums::find_by_id(&scratch.pool, holiday)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(holiday.file_count, 1);
    let art_count: u32 = sqlx::query_scalar("SELECT file_count FROM ih_categories WHERE id = ?")
        .bind(art.0)
        .fetch_one(&scratch.pool)
        .await
        .unwrap();
    assert_eq!(art_count, 1);

    scratch.drop_db().await;
}

#[tokio::test]
async fn deletion_is_retained_with_who_and_why_until_purged() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let moderator = user(&scratch.pool, "mod").await;
    let file = ready(&scratch.pool, &image(Some(alice.id))).await;

    let first = deletion(Some(moderator.id), DeleteReason::Moderation);
    assert!(
        files::soft_delete(&scratch.pool, file.id, &first)
            .await
            .unwrap()
    );
    assert!(
        !files::soft_delete(
            &scratch.pool,
            file.id,
            &deletion(Some(alice.id), DeleteReason::Owner)
        )
        .await
        .unwrap(),
        "a second delete reported success"
    );

    let kept = files::find_by_id(&scratch.pool, file.id)
        .await
        .unwrap()
        .expect("a deleted file keeps its row");
    assert_eq!(
        kept.deletion,
        Some(first),
        "the first deletion record stands"
    );
    assert!(!kept.is_live(now()));

    let alice_after = reload_user(&scratch.pool, alice.id).await;
    assert_eq!(alice_after.file_count, 0, "uncounted at soft delete");
    assert_eq!(alice_after.disk_used, 0);

    let owner_view = query(
        FileScope::User(alice.id),
        Visibility::Owner,
        FileSort::Newest,
    );
    assert!(ids(&scratch.pool, &owner_view).await.is_empty());
    let audit = query(FileScope::Everyone, Visibility::Deleted, FileSort::Newest);
    assert_eq!(ids(&scratch.pool, &audit).await, vec![file.id]);

    let purged = files::purge(&scratch.pool, file.id)
        .await
        .unwrap()
        .expect("purge returns what it removed");
    assert_eq!(purged.storage_path, file.storage_path);
    assert!(
        files::find_by_id(&scratch.pool, file.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        files::purge(&scratch.pool, file.id)
            .await
            .unwrap()
            .is_none()
    );

    let alice_after = reload_user(&scratch.pool, alice.id).await;
    assert_eq!(
        (alice_after.file_count, alice_after.disk_used),
        (0, 0),
        "purging a deleted file must not uncount it a second time"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn purging_a_live_file_uncounts_it() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let keep = ready(&scratch.pool, &image(Some(alice.id))).await;
    let gone = ready(&scratch.pool, &image(Some(alice.id))).await;

    files::purge(&scratch.pool, gone.id).await.unwrap().unwrap();

    let alice = reload_user(&scratch.pool, alice.id).await;
    assert_eq!((alice.file_count, alice.disk_used), (1, 1100));
    assert!(
        files::find_by_id(&scratch.pool, keep.id)
            .await
            .unwrap()
            .is_some()
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn duplicates_are_scoped_per_owner_and_across_guests() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let bob = user(&scratch.pool, "bob").await;
    let checksum = image(None).source_checksum.unwrap();

    let alices = ready(&scratch.pool, &image(Some(alice.id))).await;
    let guests = ready(&scratch.pool, &image(None)).await;

    let found = |owner| files::find_duplicate(&scratch.pool, checksum, owner);
    assert_eq!(
        found(Some(alice.id)).await.unwrap().map(|f| f.id),
        Some(alices.id)
    );
    assert_eq!(found(None).await.unwrap().map(|f| f.id), Some(guests.id));
    assert!(
        found(Some(bob.id)).await.unwrap().is_none(),
        "crossed owners"
    );

    files::soft_delete(
        &scratch.pool,
        alices.id,
        &deletion(Some(alice.id), DeleteReason::Owner),
    )
    .await
    .unwrap();
    assert!(
        found(Some(alice.id)).await.unwrap().is_none(),
        "a deleted file was offered as the existing copy"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn listed_visibility_applies_every_public_rule() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let hermit = user(&scratch.pool, "hermit").await;
    sqlx::query("UPDATE ih_users SET is_private = 1 WHERE id = ?")
        .bind(hermit.id.0)
        .execute(&scratch.pool)
        .await
        .unwrap();
    let public_album = album(&scratch.pool, &alice, AlbumPrivacy::Public).await;
    let private_album = album(&scratch.pool, &alice, AlbumPrivacy::Private).await;
    let unlisted_album = album(&scratch.pool, &alice, AlbumPrivacy::Unlisted).await;

    let shown = ready(&scratch.pool, &image(Some(alice.id))).await;
    let in_public = ready(
        &scratch.pool,
        &NewFile {
            album_id: Some(public_album),
            ..image(Some(alice.id))
        },
    )
    .await;
    let guest = ready(&scratch.pool, &image(None)).await;
    let in_private = ready(
        &scratch.pool,
        &NewFile {
            album_id: Some(private_album),
            ..image(Some(alice.id))
        },
    )
    .await;
    let in_unlisted = ready(
        &scratch.pool,
        &NewFile {
            album_id: Some(unlisted_album),
            ..image(Some(alice.id))
        },
    )
    .await;
    let unapproved = ready(
        &scratch.pool,
        &NewFile {
            is_approved: false,
            ..image(Some(alice.id))
        },
    )
    .await;
    let private_profile = ready(&scratch.pool, &image(Some(hermit.id))).await;
    let nsfw = ready(
        &scratch.pool,
        &NewFile {
            nsfw: true,
            ..image(Some(alice.id))
        },
    )
    .await;
    let expired = ready(
        &scratch.pool,
        &NewFile {
            expires_at: Some(now() + Duration::seconds(30)),
            ..image(Some(alice.id))
        },
    )
    .await;

    let mut listed = query(FileScope::Everyone, Visibility::Listed, FileSort::Oldest);
    listed.include_nsfw = false;
    listed.now = now() + Duration::minutes(1);
    let mut got = ids(&scratch.pool, &listed).await;
    got.sort();
    let mut want = vec![shown.id, in_public.id, guest.id];
    want.sort();
    assert_eq!(got, want);

    listed.include_nsfw = true;
    assert!(ids(&scratch.pool, &listed).await.contains(&nsfw.id));

    // The owner sees everything of theirs that is live, approved or not, in
    // any album — but an expired file is gone for everyone.
    let mut owner = query(
        FileScope::User(alice.id),
        Visibility::Owner,
        FileSort::Oldest,
    );
    owner.now = listed.now;
    let owned = ids(&scratch.pool, &owner).await;
    for id in [in_private.id, in_unlisted.id, unapproved.id, nsfw.id] {
        assert!(owned.contains(&id), "{id:?} missing from the owner's view");
    }
    assert!(!owned.contains(&expired.id), "an expired file was listed");

    // Someone holding the link to the private album sees its approved files:
    // album privacy gates the album, not the viewer who was let in.
    let viewer = query(
        FileScope::Album(private_album),
        Visibility::Viewer,
        FileSort::Oldest,
    );
    assert_eq!(ids(&scratch.pool, &viewer).await, vec![in_private.id]);
    assert!(
        !ids(&scratch.pool, &listed)
            .await
            .contains(&private_profile.id)
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn pages_never_skip_or_repeat_even_when_timestamps_tie() {
    // Every row here is created in the same second, so created_at ties
    // throughout and only the id tie-breaker keeps the cursor exact.
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let mut created = Vec::new();
    for _ in 0..23 {
        created.push(ready(&scratch.pool, &image(Some(alice.id))).await.id);
    }
    sqlx::query("UPDATE ih_files SET created_at = '2026-10-21 12:00:00'")
        .execute(&scratch.pool)
        .await
        .unwrap();

    for (sort, mut expected) in [
        (
            FileSort::Newest,
            created.iter().rev().copied().collect::<Vec<_>>(),
        ),
        (FileSort::Oldest, created.clone()),
    ] {
        let mut q = query(FileScope::User(alice.id), Visibility::Owner, sort);
        q.limit = 5;
        let mut seen = Vec::new();
        loop {
            let page = files::list(&scratch.pool, &q).await.unwrap();
            assert!(page.items.len() <= 5);
            seen.extend(page.items.iter().map(|f| f.id));
            match page.next {
                Some(next) => {
                    // Through the text form, as a client would hand it back.
                    q.after = Some(Cursor::parse(&next.to_string(), sort).unwrap());
                }
                None => break,
            }
        }
        assert_eq!(seen, std::mem::take(&mut expected), "{sort:?}");
    }

    scratch.drop_db().await;
}

#[tokio::test]
async fn popularity_orders_by_views_and_likes() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let bob = user(&scratch.pool, "bob").await;
    let quiet = ready(&scratch.pool, &image(Some(alice.id))).await;
    let viewed = ready(&scratch.pool, &image(Some(alice.id))).await;
    let liked = ready(&scratch.pool, &image(Some(alice.id))).await;

    for _ in 0..3 {
        files::increment_views(&scratch.pool, viewed.id)
            .await
            .unwrap();
    }
    files::increment_views(&scratch.pool, liked.id)
        .await
        .unwrap();
    assert!(
        files::like(&scratch.pool, alice.id, liked.id)
            .await
            .unwrap()
    );
    assert!(files::like(&scratch.pool, bob.id, liked.id).await.unwrap());
    assert!(
        !files::like(&scratch.pool, bob.id, liked.id).await.unwrap(),
        "a second like counted"
    );
    assert!(files::like(&scratch.pool, bob.id, quiet.id).await.unwrap());
    assert!(
        files::unlike(&scratch.pool, bob.id, quiet.id)
            .await
            .unwrap()
    );
    assert!(
        !files::unlike(&scratch.pool, bob.id, quiet.id)
            .await
            .unwrap()
    );

    let scope = FileScope::User(alice.id);
    assert_eq!(
        ids(
            &scratch.pool,
            &query(scope, Visibility::Owner, FileSort::MostViewed)
        )
        .await,
        vec![viewed.id, liked.id, quiet.id]
    );
    assert_eq!(
        ids(
            &scratch.pool,
            &query(scope, Visibility::Owner, FileSort::MostLiked)
        )
        .await,
        vec![liked.id, viewed.id, quiet.id],
        "equal like counts fall back to newest id first"
    );

    let mut q = query(scope, Visibility::Owner, FileSort::MostViewed);
    q.limit = 1;
    let first = files::list(&scratch.pool, &q).await.unwrap();
    q.after = first.next;
    assert_eq!(
        files::list(&scratch.pool, &q).await.unwrap().items[0].id,
        liked.id,
        "a views cursor resumes after the right row"
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn albums_keep_a_manual_order() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let holiday = album(&scratch.pool, &alice, AlbumPrivacy::Public).await;
    let in_album = |_: ()| NewFile {
        album_id: Some(holiday),
        ..image(Some(alice.id))
    };
    let a = ready(&scratch.pool, &in_album(())).await;
    let b = ready(&scratch.pool, &in_album(())).await;
    let c = ready(&scratch.pool, &in_album(())).await;
    let loose = ready(&scratch.pool, &image(Some(alice.id))).await;
    assert_eq!(
        [a.album_position, b.album_position, c.album_position],
        [Some(1), Some(2), Some(3)],
        "uploads append to the album's order"
    );

    let order = query(
        FileScope::Album(holiday),
        Visibility::Owner,
        FileSort::AlbumOrder,
    );
    assert_eq!(ids(&scratch.pool, &order).await, vec![a.id, b.id, c.id]);

    // Partial reorder: the named files come first, the rest keep their order;
    // an id from outside the album is ignored.
    files::set_album_order(&scratch.pool, holiday, &[c.id, loose.id])
        .await
        .unwrap();
    assert_eq!(ids(&scratch.pool, &order).await, vec![c.id, a.id, b.id]);

    // A file named twice, not side by side, still appears once.
    files::set_album_order(&scratch.pool, holiday, &[b.id, c.id, b.id])
        .await
        .unwrap();
    assert_eq!(ids(&scratch.pool, &order).await, vec![b.id, c.id, a.id]);
    files::set_album_order(&scratch.pool, holiday, &[c.id, a.id, b.id])
        .await
        .unwrap();

    assert!(
        files::move_to_album(&scratch.pool, loose.id, Some(holiday))
            .await
            .unwrap()
    );
    assert_eq!(
        ids(&scratch.pool, &order).await,
        vec![c.id, a.id, b.id, loose.id]
    );
    let holiday_album = albums::find_by_id(&scratch.pool, holiday)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(holiday_album.file_count, 4);

    assert!(
        files::move_to_album(&scratch.pool, a.id, None)
            .await
            .unwrap()
    );
    let moved_out = files::find_by_id(&scratch.pool, a.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((moved_out.album_id, moved_out.album_position), (None, None));
    let holiday_album = albums::find_by_id(&scratch.pool, holiday)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(holiday_album.file_count, 3);

    let outside = query(
        FileScope::User(alice.id),
        Visibility::Owner,
        FileSort::AlbumOrder,
    );
    assert!(matches!(
        files::list(&scratch.pool, &outside).await,
        Err(DbError::Invalid { field: "sort", .. })
    ));
    let mut mismatched = query(
        FileScope::User(alice.id),
        Visibility::Owner,
        FileSort::Newest,
    );
    mismatched.after = Some(Cursor::new(FileSort::MostLiked, 0, 1));
    assert!(matches!(
        files::list(&scratch.pool, &mismatched).await,
        Err(DbError::Invalid {
            field: "cursor",
            ..
        })
    ));

    scratch.drop_db().await;
}

#[tokio::test]
async fn editing_details_moves_the_category_count() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let art = category(&scratch.pool, "art").await;
    let pets = category(&scratch.pool, "pets").await;
    let file = ready(
        &scratch.pool,
        &NewFile {
            category_id: Some(art),
            ..image(Some(alice.id))
        },
    )
    .await;

    let details = FileDetails {
        title: Some("Cat".into()),
        description: Some("On a mat".into()),
        nsfw: true,
        category_id: Some(pets),
    };
    assert!(
        files::update_details(&scratch.pool, file.id, &details)
            .await
            .unwrap()
    );

    let file = files::find_by_id(&scratch.pool, file.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(file.title.as_deref(), Some("Cat"));
    assert!(file.nsfw);
    assert_eq!(file.category_id, Some(pets));
    let counts: Vec<(u32,)> = sqlx::query_as("SELECT file_count FROM ih_categories ORDER BY id")
        .fetch_all(&scratch.pool)
        .await
        .unwrap();
    assert_eq!(counts, vec![(0,), (1,)]);

    scratch.drop_db().await;
}

#[tokio::test]
async fn sensitive_fields_have_their_own_lookups() {
    let scratch = migrated_or_skip!();
    let file = ready(
        &scratch.pool,
        &NewFile {
            exif: Some(r#"{"Make":"Canon"}"#),
            uploader_ip: Some("192.0.2.9".parse().unwrap()),
            delete_token: Some("0123456789abcdef0123456789abcdef"),
            ..image(None)
        },
    )
    .await;

    assert_eq!(
        files::delete_token(&scratch.pool, file.id)
            .await
            .unwrap()
            .as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );
    let exif = files::exif(&scratch.pool, file.id).await.unwrap().unwrap();
    assert!(exif.contains("Canon"), "{exif}");
    assert_eq!(
        files::uploader_ip(&scratch.pool, file.id).await.unwrap(),
        Some("192.0.2.9".parse().unwrap())
    );
    assert_eq!(
        files::delete_token(&scratch.pool, FileId(999))
            .await
            .unwrap(),
        None
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn concurrent_uploads_count_every_file() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;

    let tasks: Vec<_> = (0..30)
        .map(|_| {
            let pool = scratch.pool.clone();
            let owner = alice.id;
            tokio::spawn(async move {
                let file = files::create_pending(&pool, &image(Some(owner)), path)
                    .await
                    .expect("create");
                files::mark_ready(&pool, file.id).await.expect("ready")
            })
        })
        .collect();
    for task in tasks {
        assert!(task.await.expect("task panicked"));
    }

    let alice = reload_user(&scratch.pool, alice.id).await;
    assert_eq!(alice.file_count, 30);
    assert_eq!(alice.disk_used, 30 * 1100);

    scratch.drop_db().await;
}

#[tokio::test]
async fn an_admin_restore_undoes_a_deletion_and_recounts() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let file = ready(&scratch.pool, &image(Some(alice.id))).await;

    assert!(
        !files::restore(&scratch.pool, file.id).await.unwrap(),
        "restoring a live file reported success"
    );
    files::soft_delete(
        &scratch.pool,
        file.id,
        &deletion(Some(alice.id), DeleteReason::Owner),
    )
    .await
    .unwrap();
    assert_eq!(reload_user(&scratch.pool, alice.id).await.file_count, 0);

    assert!(files::restore(&scratch.pool, file.id).await.unwrap());
    let restored = files::find_by_id(&scratch.pool, file.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.deletion, None);
    assert!(restored.is_live(now()));
    let alice = reload_user(&scratch.pool, alice.id).await;
    assert_eq!((alice.file_count, alice.disk_used), (1, 1100));

    scratch.drop_db().await;
}
