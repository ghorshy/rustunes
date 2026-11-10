//! One-off, hidden experiment: clear an existing track's inherited artwork
//! registration with a native `update_track` that names zero artwork fields.
//!
//! Nothing here is journaled. The probe captures the phone, sends one
//! metadata-only session, captures again and prints the artwork rows of the
//! target item before and after. Its only purpose is to establish whether the
//! receiver honours the zero values; the caller must decide what to do with
//! the answer.
use anyhow::{Context, Result, ensure};
use music_protocol::catalogue::sqlite::Catalog;
use music_protocol::protocol::session::StartedOperation;
use plist::{Dictionary, Value};
use serde_json::{Map, Value as JsonValue, json};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::engine::{
    self,
    execution::{
        link::RunLink,
        native_insert::durable_write,
        removal_batch::{self, Before, Sent},
    },
    state::layout,
    verification::sql_tables::reader::{self, Cell},
};

pub(super) struct Request {
    /// Track sync IDs (hex) whose `update_track` the session carries.
    pub(super) tracks: Vec<String>,
    /// Also send `artwork_cache_id: 0`, not only `artwork_checksum: 0`.
    pub(super) clear_cache_id: bool,
    pub(super) state: PathBuf,
    pub(super) host_name: String,
    pub(super) seconds: u64,
}

struct ProbeStarted {
    operation: Vec<u8>,
    signature: [u8; 21],
}

impl StartedOperation for ProbeStarted {
    fn operation(&self) -> &[u8] {
        &self.operation
    }
    fn signature(&self) -> &[u8; 21] {
        &self.signature
    }
}

pub(super) async fn run(r: Request, engine: &engine::Engine) -> Result<()> {
    ensure!(!r.tracks.is_empty(), "the probe names no track");
    let control = engine.control(r.seconds);
    let directory = layout::new_run(&r.state)?;
    eprintln!("probe evidence: {}", directory.display());
    let (_identity, before) =
        removal_batch::capture_fresh(&directory.join("before"), &control).await?;
    let before_sql =
        removal_batch::materialize(&before.snapshot, &directory.join("sql-before"), &control)?;
    let tracks = resolve_tracks(&before, &r.tracks)?;
    let item_pids: BTreeSet<i64> = tracks.iter().map(|(_, item)| *item).collect();
    let before_tokens = token_values(&before_sql, &item_pids)?;
    let before_rows = artwork_rows(&before_sql, &item_pids, &before_tokens)?;
    durable_write(
        &directory,
        "targets.json",
        &serde_json::to_vec_pretty(&json!({
            "schema": "rustunes.artwork-probe",
            "tracks": tracks
                .iter()
                .map(|(sync, item)| json!({
                    "sync_id": format!("{sync:016x}"),
                    "item_pid": item,
                }))
                .collect::<Vec<_>>(),
            "clear_cache_id": r.clear_cache_id,
            "before_anchor": before.anchor(),
            "before": before_rows,
        }))?,
    )?;

    let operation = build_operation(&before.current.catalog, &tracks, r.clear_cache_id)?;
    let send = directory.join("send");
    let mut link = RunLink::new(directory.join("link"));
    let first = tracks[0].0;
    let sent = removal_batch::send_metadata_only(
        &before,
        &operation,
        removal_batch::Requests::Refuse(first),
        removal_batch::sha(&operation),
        &r.host_name,
        &send,
        &control,
        &mut link,
        |_ready, signature| {
            Ok(ProbeStarted {
                operation: operation.clone(),
                signature,
            })
        },
    )
    .await;
    if send.try_exists()? {
        durable_write(&send, "operation.plist", &operation)?;
    }
    let wire = match sent {
        Sent::Unsent(error) => return Err(error).context("the probe sent nothing"),
        Sent::Started(Ok(wire)) => Some(wire),
        Sent::Started(Err(error)) => {
            eprintln!("the session failed after it started: {error:#}");
            None
        }
    };
    let finished = wire.is_some();

    let (_identity, after) =
        removal_batch::capture_fresh(&directory.join("after"), &control).await?;
    let after_sql =
        removal_batch::materialize(&after.snapshot, &directory.join("sql-after"), &control)?;
    let after_tokens = token_values(&after_sql, &item_pids)?;
    let after_rows = artwork_rows(&after_sql, &item_pids, &before_tokens)?;
    let report = json!({
        "schema": "rustunes.artwork-probe-result",
        "before_anchor": before.anchor(),
        "after_anchor": after.anchor(),
        "wire": wire.as_ref().map(|(ready, terminal, operation)| json!({
            "ready_sha256": hex(ready),
            "terminal_sha256": hex(terminal),
            "operation_sha256": hex(operation),
        })),
        "before": before_rows,
        "after": after_rows,
        "before_tokens": before_tokens,
        "after_tokens": after_tokens,
    });
    durable_write(
        &directory,
        "result.json",
        &serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    ensure!(
        finished,
        "the session did not finish; the rows above are the observed state"
    );
    Ok(())
}

/// The target sync ID and the item pid whose artwork rows it owns.
fn resolve_tracks(before: &Before, requested: &[String]) -> Result<Vec<(u64, i64)>> {
    let mut tracks = Vec::new();
    for text in requested {
        let sync = u64::from_str_radix(text.trim(), 16)
            .with_context(|| format!("invalid track sync ID {text}"))?;
        ensure!(sync != 0, "track sync ID is zero");
        ensure!(
            before
                .current
                .catalog
                .tracks()
                .iter()
                .any(|track| track.sync_id == sync),
            "track {sync:016x} is not on the phone"
        );
        tracks.push((sync, before.item_pid(sync)?));
    }
    Ok(tracks)
}

fn build_operation(
    catalog: &Catalog,
    tracks: &[(u64, i64)],
    clear_cache_id: bool,
) -> Result<Vec<u8>> {
    let revision = catalog
        .persisted_media_anchor()
        .checked_add(1)
        .context("media anchor exhausted")?;
    let mut db_info = Dictionary::new();
    db_info.insert(
        "primary_container_pid".into(),
        (catalog.primary_container_pid() as i64).into(),
    );
    let mut db_update = Dictionary::new();
    db_update.insert("operation".into(), "update_db_info".into());
    db_update.insert("pid".into(), (catalog.database_pid() as i64).into());
    db_update.insert("db_info".into(), Value::Dictionary(db_info));
    let mut operations = vec![Value::Dictionary(db_update)];
    for (sync, _) in tracks {
        let mut item = Dictionary::new();
        if clear_cache_id {
            item.insert("artwork_cache_id".into(), 0u64.into());
        }
        item.insert("artwork_checksum".into(), 0u64.into());
        let mut update = Dictionary::new();
        update.insert("operation".into(), "update_track".into());
        update.insert("pid".into(), (*sync as i64).into());
        update.insert("item".into(), Value::Dictionary(item));
        operations.push(Value::Dictionary(update));
    }
    let timestamp =
        UNIX_EPOCH + Duration::from_secs(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
    let mut root = Dictionary::new();
    root.insert("revision".into(), revision.into());
    root.insert("timestamp".into(), Value::Date(timestamp.into()));
    root.insert("operations".into(), Value::Array(operations));
    let mut bytes = Vec::new();
    Value::Dictionary(root).to_writer_binary(&mut bytes)?;
    Ok(bytes)
}

/// Token strings that the target items' own registration rows name.
fn token_values(sql: &reader::Snapshot, item_pids: &BTreeSet<i64>) -> Result<BTreeSet<String>> {
    let table = sql.table("artwork_token")?;
    let pid = table.column("entity_pid")?;
    let entity = table.column("entity_type")?;
    let token = table.column("artwork_token")?;
    let mut values = BTreeSet::new();
    for row in table.rows.keys() {
        if row[entity] == Cell::Integer(0)
            && let Cell::Integer(item) = row[pid]
            && item_pids.contains(&item)
            && let Cell::Text(value) = &row[token]
        {
            values.insert(value.clone());
        }
    }
    Ok(values)
}

fn artwork_rows(
    sql: &reader::Snapshot,
    item_pids: &BTreeSet<i64>,
    tokens: &BTreeSet<String>,
) -> Result<Vec<JsonValue>> {
    let mut out = Vec::new();
    for name in ["artwork_token", "best_artwork_token"] {
        let table = sql.table(name)?;
        let pid = table.column("entity_pid")?;
        let entity = table.column("entity_type")?;
        for (row, count) in &table.rows {
            if row[entity] != Cell::Integer(0) {
                continue;
            }
            let Cell::Integer(item) = row[pid] else {
                continue;
            };
            if item_pids.contains(&item) {
                out.push(row_json(table, row, *count, name)?);
            }
        }
    }
    let table = sql.table("artwork")?;
    let token = table.column("artwork_token")?;
    for (row, count) in &table.rows {
        let Cell::Text(value) = &row[token] else {
            continue;
        };
        if tokens.contains(value) {
            out.push(row_json(table, row, *count, "artwork")?);
        }
    }
    Ok(out)
}

fn row_json(table: &reader::Table, row: &reader::Row, count: u32, name: &str) -> Result<JsonValue> {
    let mut object = Map::new();
    object.insert("table".into(), JsonValue::String(name.into()));
    object.insert("count".into(), JsonValue::from(count));
    for (column, value) in table.columns.iter().zip(row) {
        object.insert(column.clone(), serde_json::to_value(value)?);
    }
    Ok(JsonValue::Object(object))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
