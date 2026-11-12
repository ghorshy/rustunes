//! Expected complete SQL effect of one metadata-only `update_track` that clears
//! a track's own artwork registration (`artwork_cache_id: 0` and
//! `artwork_checksum: 0`; both zeros together, as E10 observed).
//!
//! The model starts from the before tables and requires every table to equal
//! the after tables exactly, apart from the effects observed for this session:
//!
//! * The target item's entity-type-0 rows leave `artwork_token` and
//!   `best_artwork_token`. The target album's own token rows may change too,
//!   because the phone may recompute its representative. Every other token row
//!   is exact.
//! * The sync properties may advance: the anchor by one, the last-synced and
//!   hero revisions, the generation id and the maintenance date. Every other
//!   property is exact.
//! * Only the target item's and its album's entity revisions may change, and a
//!   new number must lie above the before state's highest revision.
//! * A nonzero recent play or skip counter may reset to zero, as for a
//!   metadata edit. Every other statistics cell is exact.
//! * Every other table, including `artwork`, `item` and `item_extra`, is exact.
//!
//! Any other difference refuses; the caller keeps the full delta for review.
use crate::engine::planning::metadata_edit::Tables;
use crate::engine::verification::sql_tables::reader::{Budget, Cell, Row, Table};
use crate::engine::verification::{recent_counters, sql_values as typed};
use anyhow::{Context, Result, bail, ensure};
use music_protocol::catalogue::metadata_edit::decode;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

const ANCHOR: &str = "MLSyncClientSyncAnchor";
const LAST_SYNCED: &str = "MLSyncClientLastSyncedRevision";
const GENERATION: &str = "MLSyncClientGenerationID";
const HERO: &str = "MLArtistHeroImageImportDatabaseRevision";
const MAINTENANCE: &str = "MLMaintenanceTaskOperationDate";
const SCHEMA: &str = "rustunes.artwork-clear-sql";

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct VerifiedClear {
    pub(crate) schema: &'static str,
    pub(crate) target_track_id: String,
    pub(crate) item_pid: i64,
    pub(crate) album_pid: i64,
    pub(crate) base_anchor: u32,
    pub(crate) verified_anchor: u32,
    pub(crate) cleared_artwork_token_rows: usize,
    pub(crate) cleared_best_artwork_token_rows: usize,
    pub(crate) item_revision: Option<i64>,
    pub(crate) album_revision: Option<i64>,
    pub(crate) property_changes: BTreeMap<&'static str, (String, String)>,
    pub(crate) recent_counter_resets: Vec<recent_counters::Reset>,
}

fn table<'a>(tables: &'a Tables, name: &str) -> Result<&'a Table> {
    tables
        .get(name)
        .with_context(|| format!("missing table {name}"))
}

fn property<'a>(tables: &'a Tables, key: &str) -> Result<&'a str> {
    let properties = table(tables, "_MLDatabaseProperties")?;
    let row = typed::unique(properties, "key", Cell::Text(key.into()))?;
    properties.at(row, "value")?.text()
}

fn decimal(value: &str, signed: bool) -> Result<i64> {
    let n = value.parse::<i64>().context("invalid decimal property")?;
    ensure!(
        n.to_string() == value && (signed || n >= 0),
        "noncanonical decimal property"
    );
    Ok(n)
}

fn maintenance_date(value: &str) -> Result<f64> {
    ensure!(
        value.split('.').count() <= 2
            && value
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())),
        "maintenance date is not nonnegative decimal text"
    );
    let date: f64 = value.parse().context("invalid maintenance date")?;
    ensure!(date.is_finite(), "invalid maintenance date");
    Ok(date)
}

fn text(value: &str) -> Cell {
    Cell::Text(value.to_owned())
}

fn same_tables(before: &Tables, after: &Tables) -> Result<()> {
    ensure!(
        before.keys().eq(after.keys())
            && before.iter().all(|(n, t)| {
                t.columns == after[n].columns && t.definitions == after[n].definitions
            }),
        "artwork clear table set or column definitions differ"
    );
    Ok(())
}

/// The cleared track's `item_pid` and its album's `album_pid`.
fn identities(before: &Tables, sync_id: u64) -> Result<(i64, i64)> {
    let store = table(before, "item_store")?;
    let row = typed::unique(store, "sync_id", Cell::Integer(sync_id as i64))
        .context("cleared track is not in the catalogue")?;
    let item_pid = typed::int(store, row, "item_pid")?;
    ensure!(item_pid > 0, "cleared track has a nonpositive item pid");
    let items = table(before, "item")?;
    let row = items.one("item_pid", item_pid)?;
    let album_pid = typed::int(items, row, "album_pid")?;
    Ok((item_pid, album_pid))
}

fn cleared_operation(operation: &[u8], base_anchor: u32) -> Result<u64> {
    let (envelope, updates) = decode(operation)?;
    ensure!(
        envelope.revision == base_anchor.checked_add(1).context("anchor exhausted")?,
        "artwork clear revision is not the prepared anchor plus one"
    );
    ensure!(
        updates.len() == 1,
        "artwork clear session must name exactly one track"
    );
    let update = &updates[0];
    ensure!(
        update.item.len() == 2
            && update.item.get("artwork_cache_id").and_then(plist_integer) == Some(0)
            && update.item.get("artwork_checksum").and_then(plist_integer) == Some(0),
        "artwork clear session must name only artwork_cache_id and artwork_checksum, both zero"
    );
    Ok(update.sync_id)
}

fn plist_integer(value: &plist::Value) -> Option<i64> {
    value.as_signed_integer()
}

/// Clear the target item's entity-type-0 rows from one token table and let the
/// target album's own rows change. Every other row is exact.
fn absorb_token_table(
    expected: &mut Tables,
    after: &Tables,
    name: &str,
    item_pid: i64,
    album_pid: i64,
) -> Result<usize> {
    let before = table(expected, name)?;
    let observed = table(after, name)?;
    ensure!(
        before.columns == observed.columns,
        "artwork clear changed the {name} schema"
    );
    let pid = before.column("entity_pid")?;
    let entity = before.column("entity_type")?;
    let target = |row: &Row| row[entity] == Cell::Integer(0) && row[pid] == Cell::Integer(item_pid);
    let album = |row: &Row| row[entity] == Cell::Integer(4) && row[pid] == Cell::Integer(album_pid);
    let before_other: BTreeMap<Row, u32> = before
        .rows
        .iter()
        .filter(|(row, _)| !target(row) && !album(row))
        .map(|(row, count)| (row.clone(), *count))
        .collect();
    let after_other: BTreeMap<Row, u32> = observed
        .rows
        .iter()
        .filter(|(row, _)| !target(row) && !album(row))
        .map(|(row, count)| (row.clone(), *count))
        .collect();
    ensure!(
        before_other == after_other,
        "artwork clear changed unrelated {name} rows"
    );
    ensure!(
        !observed.rows.keys().any(target),
        "artwork clear left the target item's {name} rows"
    );
    let cleared = before
        .rows
        .iter()
        .filter(|(row, _)| target(row))
        .map(|(_, count)| *count as usize)
        .sum();
    let album_rows: BTreeMap<Row, u32> = observed
        .rows
        .iter()
        .filter(|(row, _)| album(row))
        .map(|(row, count)| (row.clone(), *count))
        .collect();
    let mut desired = before_other;
    desired.extend(album_rows);
    table_mut(expected, name)?.rows = desired;
    Ok(cleared)
}

fn table_mut<'a>(tables: &'a mut Tables, name: &str) -> Result<&'a mut Table> {
    tables
        .get_mut(name)
        .with_context(|| format!("missing table {name}"))
}

/// Take the after values of the sync properties the session may advance, and
/// refuse every other property change.
fn absorb_properties(
    expected: &mut Tables,
    after: &Tables,
    base_anchor: u32,
) -> Result<BTreeMap<&'static str, (String, String)>> {
    let before = table(expected, "_MLDatabaseProperties")?.clone();
    let observed = table(after, "_MLDatabaseProperties")?;
    ensure!(
        before.columns == observed.columns,
        "artwork clear changed the property schema"
    );
    let key = before.column("key")?;
    let before_keys: BTreeSet<&Cell> = before.rows.keys().map(|row| &row[key]).collect();
    let after_keys: BTreeSet<&Cell> = observed.rows.keys().map(|row| &row[key]).collect();
    ensure!(
        before_keys == after_keys,
        "artwork clear added or removed a sync property"
    );
    let mut changes = BTreeMap::new();
    let mut replacements = Vec::new();
    for (row, count) in &before.rows {
        ensure!(*count == 1, "duplicate sync property row");
        let name = before.at(row, "key")?.text()?;
        let old = before.at(row, "value")?.text()?;
        let seen = typed::unique(observed, "key", Cell::Text(name.into()))?;
        let new = observed.at(seen, "value")?.text()?;
        if new == old {
            continue;
        }
        match name {
            ANCHOR => ensure!(
                decimal(old, false)? == i64::from(base_anchor)
                    && decimal(new, false)? == i64::from(base_anchor) + 1,
                "artwork clear anchor is not the prepared anchor plus one"
            ),
            LAST_SYNCED | HERO => {
                decimal(new, false)?;
            }
            GENERATION => {
                decimal(new, true)?;
            }
            MAINTENANCE => {
                maintenance_date(new)?;
            }
            _ => bail!("artwork clear changed the sync property {name}"),
        }
        changes.insert(
            match name {
                ANCHOR => ANCHOR,
                LAST_SYNCED => LAST_SYNCED,
                GENERATION => GENERATION,
                HERO => HERO,
                MAINTENANCE => MAINTENANCE,
                _ => unreachable!(),
            },
            (old.to_owned(), new.to_owned()),
        );
        let mut desired = row.clone();
        typed::set(&before, &mut desired, "value", text(new))?;
        replacements.push((row.clone(), desired));
    }
    for (old, new) in replacements {
        typed::replace(expected, "_MLDatabaseProperties", &old, new)?;
    }
    Ok(changes)
}

/// Let only the target item's and album's revision rows change, each new
/// number above the before state's highest revision. Every other row is exact.
fn absorb_revisions(
    expected: &mut Tables,
    after: &Tables,
    item_pid: i64,
    album_pid: i64,
) -> Result<(Option<i64>, Option<i64>)> {
    let before = table(expected, "entity_revision")?.clone();
    let observed = table(after, "entity_revision")?;
    ensure!(
        before.columns == observed.columns,
        "artwork clear changed the revision schema"
    );
    let pid = before.column("entity_pid")?;
    let allowed =
        |row: &Row| row[pid] == Cell::Integer(item_pid) || row[pid] == Cell::Integer(album_pid);
    let before_other: BTreeMap<Row, u32> = before
        .rows
        .iter()
        .filter(|(row, _)| !allowed(row))
        .map(|(row, count)| (row.clone(), *count))
        .collect();
    let after_other: BTreeMap<Row, u32> = observed
        .rows
        .iter()
        .filter(|(row, _)| !allowed(row))
        .map(|(row, count)| (row.clone(), *count))
        .collect();
    ensure!(
        before_other == after_other,
        "artwork clear changed an unrelated entity revision"
    );
    let highest = before
        .rows
        .keys()
        .map(|row| before.at(row, "revision")?.integer())
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .max()
        .unwrap_or(0);
    let mut item = None;
    let mut album = None;
    let mut desired = before_other;
    for (row, count) in &observed.rows {
        if !allowed(row) {
            continue;
        }
        let now = observed.at(row, "revision")?.integer()?;
        let prior = before
            .rows
            .get(row)
            .and_then(|_| before.at(row, "revision").ok());
        if prior.and_then(|cell| cell.integer().ok()) != Some(now) {
            ensure!(
                now > highest,
                "artwork clear allocated a revision inside the existing block"
            );
        }
        if row[pid] == Cell::Integer(item_pid) {
            item = Some(now);
        }
        if row[pid] == Cell::Integer(album_pid) {
            album = Some(now);
        }
        desired.insert(row.clone(), *count);
    }
    table_mut(expected, "entity_revision")?.rows = desired;
    Ok((item, album))
}

fn describe(name: &str, expected: &Table, actual: &Table) -> String {
    let show = |t: &Table, row: &Row| {
        t.columns
            .iter()
            .zip(row)
            .map(|(c, v)| format!("{c}={v:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut lines = vec![format!("{name}:")];
    for (row, n) in &expected.rows {
        if actual.rows.get(row) != Some(n) {
            lines.push(format!("  expected only: {}", show(expected, row)));
        }
        if lines.len() > 8 {
            break;
        }
    }
    for (row, n) in &actual.rows {
        if expected.rows.get(row) != Some(n) {
            lines.push(format!("  observed only: {}", show(actual, row)));
        }
        if lines.len() > 16 {
            break;
        }
    }
    lines.join("\n")
}

/// Verify one artwork-clearing session. `operation` is the encoded session
/// bytes that were staged; `base_anchor` is the anchor it was prepared against.
pub(crate) fn verify(
    before: &Tables,
    after: &Tables,
    operation: &[u8],
    base_anchor: u32,
) -> Result<VerifiedClear> {
    same_tables(before, after)?;
    let sync_id = cleared_operation(operation, base_anchor)?;
    ensure!(
        decimal(property(before, ANCHOR)?, false)? == i64::from(base_anchor),
        "artwork clear before state is not at the prepared anchor"
    );
    let verified_anchor = decimal(property(after, ANCHOR)?, false)?;
    ensure!(
        verified_anchor == i64::from(base_anchor) + 1,
        "artwork clear after state is not at the prepared anchor plus one"
    );
    let info = table(before, "db_info")?;
    ensure!(info.rows.len() == 1, "db_info is not a single row");
    let info_row = info.rows.keys().next().context("db_info row")?;
    let (envelope, _) = decode(operation)?;
    ensure!(
        typed::int(info, info_row, "db_pid")? == envelope.database_pid as i64
            && typed::int(info, info_row, "primary_container_pid")?
                == envelope.primary_container_pid as i64,
        "artwork clear database or library container identity differs"
    );
    let (item_pid, album_pid) = identities(before, sync_id)?;

    let mut expected = before.clone();
    let cleared_artwork_token_rows =
        absorb_token_table(&mut expected, after, "artwork_token", item_pid, album_pid)?;
    let cleared_best_artwork_token_rows = absorb_token_table(
        &mut expected,
        after,
        "best_artwork_token",
        item_pid,
        album_pid,
    )?;
    let property_changes = absorb_properties(&mut expected, after, base_anchor)?;
    let (item_revision, album_revision) =
        absorb_revisions(&mut expected, after, item_pid, album_pid)?;

    recent_counters::qualify(table(before, "item_stats")?)?;
    let resets = recent_counters::absorb(
        table_mut(&mut expected, "item_stats")?,
        table(after, "item_stats")?,
        &Budget::new(),
    )?;

    let mismatches: Vec<_> = expected
        .iter()
        .filter(|(name, table)| table.rows != after[*name].rows)
        .map(|(name, table)| describe(name, table, &after[name]))
        .collect();
    ensure!(
        mismatches.is_empty(),
        "unmodelled artwork clear rows:\n{}",
        mismatches.join("\n")
    );
    Ok(VerifiedClear {
        schema: SCHEMA,
        target_track_id: format!("{sync_id:016x}"),
        item_pid,
        album_pid,
        base_anchor,
        verified_anchor: u32::try_from(verified_anchor).context("anchor is not a u32")?,
        cleared_artwork_token_rows,
        cleared_best_artwork_token_rows,
        item_revision,
        album_revision,
        property_changes,
        recent_counter_resets: resets,
    })
}

#[cfg(test)]
mod tests;
