//! Full native-schema fixtures for the artwork-clear model.
use super::*;
use crate::engine::verification::metadata_edit_sql::tests::{
    ANCHOR_BEFORE, World, num, operation, update,
};
use rusqlite::Connection;

/// The target is track 1001 (item 1, album 10); track 1002 (item 2) and album
/// 11 keep their own registrations, which the clear must leave exact.
const ARTWORK: &str = "
INSERT INTO artwork(artwork_token,artwork_source_type,relative_path,artwork_type)VALUES
    ('100',300,'aa/0100',6),('200',300,'aa/0200',6);
INSERT INTO artwork_token(entity_pid,artwork_token,artwork_source_type,artwork_type,entity_type,artwork_variant_type)VALUES
    (1,'100',300,6,0,0),(2,'100',300,6,0,0),(10,'100',300,6,4,0),(11,'200',300,6,4,0);
INSERT INTO best_artwork_token(entity_pid,entity_type,artwork_type,available_artwork_token,fetchable_artwork_source_type,artwork_variant_type)VALUES
    (1,0,6,'100',300,0),(2,0,6,'100',300,0),(10,4,6,'100',300,0),(11,4,6,'200',300,0);";

fn artwork_world() -> World {
    let world = World::new();
    for path in [&world.fixture.before, &world.fixture.after] {
        Connection::open(path)
            .unwrap()
            .execute_batch(ARTWORK)
            .unwrap();
    }
    world
}

fn clear() -> Vec<u8> {
    operation(&[update(
        1001,
        &[("artwork_cache_id", num(0)), ("artwork_checksum", num(0))],
    )])
}

/// The observed session effect: the target's own token rows leave both tables,
/// the sync properties advance, and the target item's and album's revisions
/// move above the before block.
fn cleared_sql(extra: &str) -> String {
    format!(
        "DELETE FROM artwork_token WHERE entity_pid=1 AND entity_type=0;
         DELETE FROM best_artwork_token WHERE entity_pid=1 AND entity_type=0;
         UPDATE _MLDatabaseProperties SET value='11' WHERE key='MLSyncClientSyncAnchor';
         UPDATE _MLDatabaseProperties SET value='18' WHERE key IN('MLSyncClientLastSyncedRevision','MLArtistHeroImageImportDatabaseRevision');
         UPDATE _MLDatabaseProperties SET value='-6' WHERE key='MLSyncClientGenerationID';
         UPDATE entity_revision SET revision=18 WHERE entity_pid=1 AND class=0 AND revision_type=0;
         UPDATE entity_revision SET revision=19 WHERE entity_pid=10 AND class=4 AND revision_type=0;
         {extra}"
    )
}

fn run(world: &World, after_sql: &str) -> Result<VerifiedClear> {
    run_with(world, after_sql, &clear(), ANCHOR_BEFORE)
}

fn run_with(
    world: &World,
    after_sql: &str,
    operation: &[u8],
    base_anchor: u32,
) -> Result<VerifiedClear> {
    let before = world.before();
    let after = world.after(after_sql);
    verify(&before, &after, operation, base_anchor)
}

fn refusal(world: &World, after_sql: &str) -> String {
    format!(
        "{:#}",
        run(world, after_sql).expect_err("the clear should have been refused")
    )
}

#[test]
fn a_clear_removes_the_target_rows_and_advances_the_session_properties() {
    let world = artwork_world();
    let proof = run(&world, &cleared_sql("")).unwrap();
    assert_eq!(proof.schema, "rustunes.artwork-clear-sql");
    assert_eq!(proof.target_track_id, "00000000000003e9");
    assert_eq!((proof.item_pid, proof.album_pid), (1, 10));
    assert_eq!((proof.base_anchor, proof.verified_anchor), (10, 11));
    assert_eq!(proof.cleared_artwork_token_rows, 1);
    assert_eq!(proof.cleared_best_artwork_token_rows, 1);
    assert_eq!(proof.item_revision, Some(18));
    assert_eq!(proof.album_revision, Some(19));
    assert_eq!(
        proof
            .property_changes
            .get(ANCHOR)
            .map(|(a, b)| (&**a, &**b)),
        Some(("10", "11"))
    );
    // The album may drop its own registration too, as the phone recomputes its
    // representative.
    let world = artwork_world();
    assert!(
        run(
            &world,
            &cleared_sql("DELETE FROM artwork_token WHERE entity_pid=10 AND entity_type=4;")
        )
        .is_ok()
    );
}

#[test]
fn a_clear_may_reset_a_recent_counter() {
    let world = artwork_world();
    let proof = run(
        &world,
        &cleared_sql("UPDATE item_stats SET play_count_recent=0 WHERE item_pid=1;"),
    )
    .unwrap();
    assert_eq!(proof.recent_counter_resets.len(), 1);
    assert_eq!(proof.recent_counter_resets[0].item_pid, 1);
}

#[test]
fn a_clear_refuses_a_remaining_or_changed_token_row() {
    for extra in [
        // The target's best token row is still there.
        "INSERT INTO best_artwork_token(entity_pid,entity_type,artwork_type,available_artwork_token,fetchable_artwork_source_type,artwork_variant_type)VALUES(1,0,6,'100',300,0);",
        // Another track's row changed.
        "UPDATE artwork_token SET artwork_token='200' WHERE entity_pid=2 AND entity_type=0;",
        // Another album's row changed.
        "DELETE FROM artwork_token WHERE entity_pid=11 AND entity_type=4;",
        // A row appeared for an unrelated entity.
        "INSERT INTO artwork_token(entity_pid,artwork_token,artwork_source_type,artwork_type,entity_type,artwork_variant_type)VALUES(3,'100',300,6,0,0);",
    ] {
        let world = artwork_world();
        assert!(
            refusal(&world, &cleared_sql(extra)).contains("artwork clear"),
            "{extra}"
        );
    }
}

#[test]
fn a_clear_refuses_an_unmodelled_table_property_or_revision_change() {
    for extra in [
        "UPDATE item_extra SET title='Other' WHERE item_pid=1;",
        "DELETE FROM artwork WHERE artwork_token='100';",
        "UPDATE _MLDatabaseProperties SET value='12' WHERE key='MLSyncClientSyncAnchor';",
        "UPDATE entity_revision SET revision=0 WHERE entity_pid=2 AND class=0 AND revision_type=0;",
        "UPDATE entity_revision SET revision=0 WHERE entity_pid=1 AND class=0 AND revision_type=0;",
    ] {
        let world = artwork_world();
        let error = refusal(&world, &cleared_sql(extra));
        assert!(error.contains("artwork clear"), "{extra}: {error}");
    }
}

#[test]
fn a_clear_refuses_a_wrong_operation_anchor_or_target() {
    let world = artwork_world();
    let after = cleared_sql("");
    let other = operation(&[update(1001, &[("artwork_checksum", num(0))])]);
    assert!(run_with(&world, &after, &other, ANCHOR_BEFORE).is_err());

    let world = artwork_world();
    let nonzero = operation(&[update(
        1001,
        &[("artwork_cache_id", num(1)), ("artwork_checksum", num(0))],
    )]);
    assert!(run_with(&world, &after, &nonzero, ANCHOR_BEFORE).is_err());

    let world = artwork_world();
    let two = operation(&[
        update(
            1001,
            &[("artwork_cache_id", num(0)), ("artwork_checksum", num(0))],
        ),
        update(
            1002,
            &[("artwork_cache_id", num(0)), ("artwork_checksum", num(0))],
        ),
    ]);
    assert!(run_with(&world, &after, &two, ANCHOR_BEFORE).is_err());

    let world = artwork_world();
    assert!(run_with(&world, &after, &clear(), ANCHOR_BEFORE + 1).is_err());

    // The target is not in the before catalogue.
    let world = artwork_world();
    let missing = operation(&[update(
        9999,
        &[("artwork_cache_id", num(0)), ("artwork_checksum", num(0))],
    )]);
    assert!(run_with(&world, &after, &missing, ANCHOR_BEFORE).is_err());
}

#[test]
fn a_clear_without_a_target_row_is_a_no_op_clear() {
    let world = World::new();
    let proof = run(&world, &cleared_sql("")).unwrap();
    assert_eq!(proof.cleared_artwork_token_rows, 0);
    assert_eq!(proof.cleared_best_artwork_token_rows, 0);
}
