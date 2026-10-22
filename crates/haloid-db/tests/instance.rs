//! The instance row, against a real MariaDB.

#[macro_use]
mod common;

use haloid_db::instance;

#[tokio::test]
async fn the_first_id_sticks_and_a_key_can_be_installed_and_removed() {
    let scratch = migrated_or_skip!();
    let db = &scratch.pool;

    assert_eq!(
        instance::get(db).await.unwrap(),
        None,
        "nothing before the first start"
    );
    let first = instance::ensure(db, "inst_first").await.unwrap();
    assert_eq!(instance::get(db).await.unwrap(), Some(first.clone()));
    assert_eq!(first.instance_id, "inst_first");
    assert_eq!(first.licence_key, None);
    // A later start, or a second process, keeps the id already made.
    let again = instance::ensure(db, "inst_second").await.unwrap();
    assert_eq!(again.instance_id, "inst_first");

    instance::set_licence_key(db, Some("HALOID1.a.b"))
        .await
        .unwrap();
    assert_eq!(
        instance::ensure(db, "x")
            .await
            .unwrap()
            .licence_key
            .as_deref(),
        Some("HALOID1.a.b")
    );
    instance::set_licence_key(db, None).await.unwrap();
    assert_eq!(instance::ensure(db, "x").await.unwrap().licence_key, None);

    scratch.drop_db().await;
}
