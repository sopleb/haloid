//! The tags and categories repositories, against a real MariaDB.

mod common;

use chrono::{SubsecRound, Utc};
use haloid_core::account::{User, UserStatus};
use haloid_core::auth::Role;
use haloid_core::files::{DeleteReason, Deletion};
use haloid_core::ids::{FileId, UserId};
use haloid_core::media::{Chain, MediaKind};
use haloid_core::pagination::FileSort;
use haloid_core::storage::StoragePathMode;
use haloid_db::categories::{self, CategoryDetails};
use haloid_db::files::{self, FileQuery, FileScope, NewFile, Visibility};
use haloid_db::tags;
use haloid_db::users::{self, NewUser};
use haloid_db::{Db, DbError};

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

async fn ready_file(db: &Db, owner: UserId) -> FileId {
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
        album_id: None,
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

fn details(name: &str, key: &str) -> CategoryDetails {
    CategoryDetails {
        name: name.into(),
        url_key: key.into(),
        description: None,
        sort_order: 0,
    }
}

async fn tag_count(db: &Db, slug: &str) -> u32 {
    tags::find_by_slug(db, slug)
        .await
        .unwrap()
        .map_or(0, |t| t.file_count)
}

#[tokio::test]
async fn tagging_upserts_by_slug_and_counts_files() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let one = ready_file(&scratch.pool, alice.id).await;
    let two = ready_file(&scratch.pool, alice.id).await;

    let set = tags::set_for_file(&scratch.pool, one, &["Cats", "black and white"])
        .await
        .unwrap();
    let slugs: Vec<&str> = set.iter().map(|t| t.slug.as_str()).collect();
    assert_eq!(slugs, vec!["cats", "black-and-white"], "as written");

    // A different spelling of an existing tag is the same tag, and keeps its
    // first spelling as its name.
    tags::set_for_file(&scratch.pool, two, &["CATS", "  cats ", "#!"])
        .await
        .unwrap();
    let cats = tags::find_by_slug(&scratch.pool, "cats")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cats.name, "Cats");
    assert_eq!(cats.file_count, 2);

    // Replacing a file's tags moves the counts.
    tags::set_for_file(&scratch.pool, one, &["dogs"])
        .await
        .unwrap();
    assert_eq!(tag_count(&scratch.pool, "cats").await, 1);
    assert_eq!(tag_count(&scratch.pool, "black-and-white").await, 0);
    assert_eq!(tag_count(&scratch.pool, "dogs").await, 1);
    let ones: Vec<String> = tags::for_file(&scratch.pool, one)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.slug)
        .collect();
    assert_eq!(ones, vec!["dogs"]);

    scratch.drop_db().await;
}

#[tokio::test]
async fn tag_counts_follow_deletion_restore_and_purge() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let file = ready_file(&scratch.pool, alice.id).await;
    tags::set_for_file(&scratch.pool, file, &["cats"])
        .await
        .unwrap();

    let deletion = Deletion {
        at: Utc::now().trunc_subsecs(0),
        by: Some(alice.id),
        reason: DeleteReason::Owner,
    };
    files::soft_delete(&scratch.pool, file, &deletion)
        .await
        .unwrap();
    assert_eq!(tag_count(&scratch.pool, "cats").await, 0);

    files::restore(&scratch.pool, file).await.unwrap();
    assert_eq!(tag_count(&scratch.pool, "cats").await, 1);

    files::purge(&scratch.pool, file).await.unwrap();
    assert_eq!(tag_count(&scratch.pool, "cats").await, 0);

    scratch.drop_db().await;
}

#[tokio::test]
async fn files_can_be_listed_by_tag_and_category_and_tags_by_popularity() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let art = categories::create(&scratch.pool, &details("Art", "art"))
        .await
        .unwrap();
    let a = ready_file(&scratch.pool, alice.id).await;
    let b = ready_file(&scratch.pool, alice.id).await;
    let c = ready_file(&scratch.pool, alice.id).await;
    tags::set_for_file(&scratch.pool, a, &["cats", "art"])
        .await
        .unwrap();
    tags::set_for_file(&scratch.pool, b, &["cats"])
        .await
        .unwrap();
    files::update_details(
        &scratch.pool,
        c,
        &files::FileDetails {
            title: None,
            description: None,
            nsfw: false,
            category_id: Some(art.id),
        },
    )
    .await
    .unwrap();

    let cats = tags::find_by_slug(&scratch.pool, "cats")
        .await
        .unwrap()
        .unwrap();
    let list = |scope| FileQuery {
        scope,
        visibility: Visibility::Listed,
        include_nsfw: true,
        kind: None,
        sort: FileSort::Oldest,
        after: None,
        limit: 10,
        now: Utc::now(),
    };
    let ids = |page: haloid_core::pagination::Page<_, _>| -> Vec<FileId> {
        page.items
            .into_iter()
            .map(|f: haloid_core::files::MediaFile| f.id)
            .collect()
    };
    assert_eq!(
        ids(files::list(&scratch.pool, &list(FileScope::Tag(cats.id)))
            .await
            .unwrap()),
        vec![a, b]
    );
    assert_eq!(
        ids(
            files::list(&scratch.pool, &list(FileScope::Category(art.id)))
                .await
                .unwrap()
        ),
        vec![c]
    );

    let popular: Vec<String> = tags::popular(&scratch.pool, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.slug)
        .collect();
    assert_eq!(popular, vec!["cats", "art"]);

    scratch.drop_db().await;
}

#[tokio::test]
async fn categories_are_curated_with_unique_keys() {
    let scratch = migrated_or_skip!();

    let mut pets = details("Pets", "pets");
    pets.sort_order = 2;
    let pets = categories::create(&scratch.pool, &pets).await.unwrap();
    let mut art = details("Art", "art");
    art.sort_order = 1;
    let art = categories::create(&scratch.pool, &art).await.unwrap();

    assert!(matches!(
        categories::create(&scratch.pool, &details("Pets again", "PETS")).await,
        Err(DbError::Conflict("url_key"))
    ));

    let keys: Vec<String> = categories::list(&scratch.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.url_key)
        .collect();
    assert_eq!(keys, vec!["art", "pets"], "sort_order, then name");

    let mut renamed = details("Animals", "animals");
    renamed.description = Some("Furry".into());
    assert!(
        categories::update(&scratch.pool, pets.id, &renamed)
            .await
            .unwrap()
    );
    assert!(matches!(
        categories::update(&scratch.pool, pets.id, &details("Art", "art")).await,
        Err(DbError::Conflict("url_key"))
    ));
    let animals = categories::find_by_url_key(&scratch.pool, "animals")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(animals.id, pets.id);
    assert_eq!(animals.description.as_deref(), Some("Furry"));

    assert!(categories::delete(&scratch.pool, art.id).await.unwrap());
    assert!(
        categories::find_by_id(&scratch.pool, art.id)
            .await
            .unwrap()
            .is_none()
    );

    scratch.drop_db().await;
}

#[tokio::test]
async fn deleting_a_category_unfiles_its_files() {
    let scratch = migrated_or_skip!();
    let alice = user(&scratch.pool, "alice").await;
    let art = categories::create(&scratch.pool, &details("Art", "art"))
        .await
        .unwrap();
    let file = ready_file(&scratch.pool, alice.id).await;
    files::update_details(
        &scratch.pool,
        file,
        &files::FileDetails {
            title: None,
            description: None,
            nsfw: false,
            category_id: Some(art.id),
        },
    )
    .await
    .unwrap();

    categories::delete(&scratch.pool, art.id).await.unwrap();
    let file = files::find_by_id(&scratch.pool, file)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(file.category_id, None);
    assert!(file.is_live(Utc::now()));

    scratch.drop_db().await;
}

#[tokio::test]
async fn tags_come_back_in_the_order_they_were_written() {
    let scratch = migrated_or_skip!();
    let owner = user(&scratch.pool, "mira").await;
    let file = ready_file(&scratch.pool, owner.id).await;
    let names = |tags: Vec<haloid_core::taxonomy::Tag>| {
        tags.into_iter().map(|t| t.name).collect::<Vec<_>>()
    };

    let set = tags::set_for_file(&scratch.pool, file, &["sunset", "Beach", "2024", "beach"])
        .await
        .unwrap();
    assert_eq!(names(set), ["sunset", "Beach", "2024"]);
    // Reordered, with one dropped and one added: the new order holds.
    tags::set_for_file(&scratch.pool, file, &["2024", "harbour", "sunset"])
        .await
        .unwrap();
    assert_eq!(
        names(tags::for_file(&scratch.pool, file).await.unwrap()),
        ["2024", "harbour", "sunset"]
    );

    scratch.drop_db().await;
}
