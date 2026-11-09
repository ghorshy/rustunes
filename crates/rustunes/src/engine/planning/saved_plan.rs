//! Saved plans are untrusted descriptions. Recompute every decision from the
//! complete snapshot, current sources and journal before accepting one. The
//! one exception is a plan this run made itself ([`Planned`]): while its file,
//! the snapshot, the journal and every source's stat are unchanged, its
//! decisions are taken as made. Sending still checks every source's bytes.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    engine::planning::additive::{self, Candidate, Decision, Plan},
    engine::planning::collections::CandidateIds,
    engine::planning::source_collection::{self, Selection, SelectionOptions, SourceCollection},
    engine::state::journal::{Identity, Journal},
    engine::state::snapshot,
};
use music_protocol::catalogue::identity;

mod unique_json;

pub(super) fn unique_value(bytes: &[u8]) -> Result<Value> {
    Ok(serde_json::from_slice::<unique_json::UniqueJson>(bytes)?.0)
}

pub struct Validated {
    pub plan: Plan,
    pub plan_sha256: [u8; 32],
    pub snapshot: snapshot::Snapshot,
    pub sources: SourceCollection,
    pub identity: Identity,
    pub journal: Option<Journal>,
}

/// A plan made earlier in this run and what it was made from, handed on so
/// that checking it again inspects and plans nothing a second time. It is
/// taken only while the plan file, the snapshot, the journal and every
/// selected source are as they were; otherwise the check inspects and plans
/// again.
#[derive(Clone)]
pub struct Planned {
    plan: Plan,
    sources: SourceCollection,
    basis: Basis,
}

/// What a plan was made from, as far as this process can tell it apart.
#[derive(Clone, PartialEq, Eq)]
struct Basis {
    plan_sha256: [u8; 32],
    snapshot: PathBuf,
    snapshot_receipt_sha256: [u8; 32],
    journal_sha256: Option<[u8; 32]>,
    /// Each selected file, and its stat when it was selected for inspection.
    selected: Vec<(PathBuf, Stat)>,
    /// What the selection left out, which the plan lists too.
    excluded: Vec<source_collection::Exclusion>,
}

impl Basis {
    /// The plan saved as `plan`, made from the snapshot at `snapshot_path`,
    /// `journal` and the sources in `selection`.
    fn of(
        plan: &[u8],
        snapshot_path: &Path,
        snapshot: &snapshot::Snapshot,
        journal: Option<&Journal>,
        selection: &Selection,
    ) -> Result<Self> {
        Ok(Self {
            plan_sha256: Sha256::digest(plan).into(),
            snapshot: fs::canonicalize(snapshot_path)?,
            snapshot_receipt_sha256: snapshot.index_receipt_sha256,
            journal_sha256: journal.map(Journal::recheck_read_only_source).transpose()?,
            selected: selection
                .files()
                .iter()
                .map(|file| (file.path().to_path_buf(), Stat::of(file.metadata())))
                .collect(),
            excluded: selection.exclusions().to_vec(),
        })
    }
}

/// What a file's stat says about its identity and content in this process.
#[derive(Clone, PartialEq, Eq)]
struct Stat {
    dev: u64,
    ino: u64,
    size: u64,
    /// Seconds and nanoseconds.
    modified: (i64, i64),
    /// Seconds and nanoseconds.
    changed: (i64, i64),
}

impl Stat {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            size: metadata.size(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

impl Planned {
    /// `plan`, saved as `serialized`, made from the snapshot at
    /// `snapshot_path`, `journal` and the `sources` inspected from `selection`.
    pub(crate) fn new(
        plan: &Plan,
        serialized: &[u8],
        snapshot_path: &Path,
        snapshot: &snapshot::Snapshot,
        journal: Option<&Journal>,
        selection: &Selection,
        sources: &SourceCollection,
    ) -> Result<Self> {
        Ok(Self {
            plan: plan.clone(),
            sources: sources.clone(),
            basis: Basis::of(serialized, snapshot_path, snapshot, journal, selection)?,
        })
    }

    /// The plan and its sources, if `basis` is what they were made from.
    fn reused(&self, basis: &Basis) -> Option<(Plan, SourceCollection)> {
        (*basis == self.basis).then(|| (self.plan.clone(), self.sources.clone()))
    }
}

/// Validate a plan without journal recovery or journal-mode changes. Only a
/// later real apply may reopen the journal for transitions and durable writes.
pub fn load_read_only(
    plan_path: &Path,
    snapshot_path: &Path,
    journal_path: &Path,
) -> Result<Validated> {
    load_with_access(plan_path, snapshot_path, journal_path, None, false, None)
}

/// Controlled mixed execution keeps the original deadline through source
/// inspection, managed context derivation and exact saved equality. A plan
/// made `earlier` in this run is taken as it is while nothing it was made
/// from changed ([`Planned`]).
pub(crate) fn load_read_only_with_control(
    plan_path: &Path,
    snapshot_path: &Path,
    journal_path: &Path,
    control: &crate::engine::execution::native_insert::Control,
    earlier: Option<&Planned>,
) -> Result<Validated> {
    control.check()?;
    load_with_access(
        plan_path,
        snapshot_path,
        journal_path,
        Some(control),
        false,
        earlier,
    )
}

/// Sync's initial cover/source expectations are inspection only. The later
/// actual Apply loader still independently reconstructs and gates execution.
pub(crate) fn load_sync_expectations(
    plan_path: &Path,
    snapshot_path: &Path,
    journal_path: &Path,
    earlier: Option<&Planned>,
) -> Result<Validated> {
    load_with_access(plan_path, snapshot_path, journal_path, None, true, earlier)
}

fn load_with_access(
    plan_path: &Path,
    snapshot_path: &Path,
    journal_path: &Path,
    control: Option<&crate::engine::execution::native_insert::Control>,
    description_only: bool,
    earlier: Option<&Planned>,
) -> Result<Validated> {
    let _phase = crate::engine::phase::start(crate::engine::phase::Phase::CheckSavedPlan);
    if let Some(control) = control {
        control.check()?;
    }
    let bytes = read_stable(plan_path)?;
    let saved = serde_json::from_slice::<unique_json::UniqueJson>(&bytes)
        .context("reading saved JSON plan with unique object keys")?
        .0;
    ensure!(
        saved["schema"] == "rustunes.additive-plan"
            && saved["mode"] == "additive_dry_run"
            && saved["phone_deletions"] == 0
            && saved["execution_authorized_by_plan"] == false,
        "unsupported saved plan schema or mode"
    );
    let snapshot = match control {
        Some(control) => snapshot::open_with_control(snapshot_path, control)?,
        None => snapshot::open(snapshot_path)?,
    };
    let library = identity::parse(&snapshot.raw_preferences)?;
    let identity =
        Identity::for_saved_device(snapshot.identity.device_identifier_sha256, &library)?;
    let journal = if journal_path.try_exists()? {
        // Recompute and gate the entire description before any writable open.
        Some(Journal::open_read_only(journal_path, &identity)?)
    } else {
        None
    };
    let scope = &saved["selection"];
    let options = SelectionOptions {
        recursive: scope["recursive"]
            .as_bool()
            .context("missing recursive selection")?,
        include: strings(&scope["include"])?,
        exclude: strings(&scope["exclude"])?,
    };
    let root = Path::new(
        saved["source_root"]
            .as_str()
            .context("missing source root")?,
    );
    ensure!(root.is_absolute(), "saved source root must be absolute");
    let selection = source_collection::select(root, &options)?;
    let reused = match earlier {
        Some(earlier) => earlier.reused(&Basis::of(
            &bytes,
            snapshot_path,
            &snapshot,
            journal.as_ref(),
            &selection,
        )?),
        None => None,
    };
    let (plan, sources) = match reused {
        Some(reused) => reused,
        None => plan_again(
            &saved,
            &selection,
            &snapshot,
            journal.as_ref(),
            &identity,
            control,
        )?,
    };
    if let Some(control) = control {
        control.check()?;
    }
    if description_only {
        ensure_description_recomputed(&plan, &saved)?;
    } else {
        ensure_recomputed(&plan, &saved)?;
    }
    if let Some(control) = control {
        control.check()?;
    }
    Ok(Validated {
        plan,
        plan_sha256: Sha256::digest(&bytes).into(),
        snapshot,
        sources,
        identity,
        journal,
    })
}

/// Inspect `selection` and make the plan `saved` describes again, from its
/// additions' identifiers and policies.
fn plan_again(
    saved: &Value,
    selection: &Selection,
    snapshot: &snapshot::Snapshot,
    journal: Option<&Journal>,
    identity: &Identity,
    control: Option<&crate::engine::execution::native_insert::Control>,
) -> Result<(Plan, SourceCollection)> {
    let mut last = std::time::Instant::now();
    let mut sources = source_collection::inspect_with_progress(selection, |done, total| {
        if let Some(control) = control {
            control.check()?;
        }
        if done == 0 || done == total || last.elapsed().as_secs() >= 5 {
            crate::engine::observe(crate::engine::Event::Observation(
                crate::engine::Observation::RevalidatingSources { done, total },
            ))?;
            last = std::time::Instant::now();
        }
        Ok(())
    })?;
    let mut candidates = BTreeMap::new();
    let rows = saved["sources"]
        .as_array()
        .context("missing source decisions")?;
    ensure!(rows.len() <= 4096, "saved plan exceeds source bound");
    for row in rows {
        if row["decision"] != "add" {
            continue;
        }
        let name = row["relative_path"]
            .as_str()
            .context("missing source path")?;
        ensure!(
            candidates
                .insert(
                    name.to_owned(),
                    Candidate {
                        ids: CandidateIds {
                            track: pid(&row["track_id"])?,
                            album: Some(pid(&row["album"]["id"])?),
                            artist: Some(pid(&row["artist"]["id"])?),
                        },
                        path: row["path"]
                            .as_str()
                            .context("missing destination")?
                            .to_owned(),
                    }
                )
                .is_none(),
            "duplicate addition source"
        );
    }
    sources.flac_to_alac = match saved.get("flac_to_alac") {
        None => false,
        Some(value) => value.as_bool().context("invalid FLAC conversion policy")?,
    };
    let preserve_existing = match saved.get("preserve_existing_tracks") {
        None => false,
        Some(value) => value
            .as_bool()
            .context("invalid existing-track preservation policy")?,
    };
    let evidence = || journal.map(|journal| additive::JournalEvidence { journal, identity });
    let plan = match control {
        Some(control) => additive::build_from_snapshot_with_control(
            &sources,
            snapshot,
            evidence(),
            &candidates,
            preserve_existing,
            control,
        )?,
        None => additive::build_from_snapshot(
            &sources,
            snapshot,
            evidence(),
            &candidates,
            preserve_existing,
        )?,
    };
    Ok((plan, sources))
}

/// Exact reconstruction is separate from permission to apply a described action.
pub(crate) fn ensure_description_recomputed(plan: &additive::Plan, saved: &Value) -> Result<()> {
    ensure!(
        serde_json::to_value(plan)? == *saved,
        "saved plan differs from current sources, snapshot, journal or planner; generate a new plan"
    );
    Ok(())
}

// The exact-description and independent execution gates run before any connector.
pub(crate) fn ensure_recomputed(plan: &additive::Plan, saved: &Value) -> Result<()> {
    ensure_description_recomputed(plan, saved)?;
    ensure!(
        !plan.has_conflicts(),
        "saved plan contains conflicts; resolve them or select a smaller scope"
    );
    ensure!(
        plan.sources.iter().all(|row| matches!(
            row.decision,
            Decision::Add { .. } | Decision::Update { .. } | Decision::Keep | Decision::Skip { .. }
        )),
        "saved plan contains an unsupported operation"
    );
    Ok(())
}

fn strings(value: &Value) -> Result<BTreeSet<String>> {
    let values = value.as_array().context("missing source selectors")?;
    ensure!(values.len() <= 4096, "source selector bound exceeded");
    let mut result = BTreeSet::new();
    for value in values {
        ensure!(
            result.insert(value.as_str().context("selector must be text")?.to_owned()),
            "duplicate source selector"
        );
    }
    Ok(result)
}

pub fn pid(value: &Value) -> Result<u64> {
    let text = value.as_str().context("track/group ID missing")?;
    ensure!(
        text.len() == 16
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "IDs must contain exactly 16 lowercase hexadecimal digits"
    );
    let id = u64::from_str_radix(text, 16)?;
    ensure!(id >= 1000, "reserved track/group ID");
    Ok(id)
}

pub(super) fn read_stable(path: &Path) -> Result<Vec<u8>> {
    let before = fs::symlink_metadata(path)?;
    ensure!(
        before.is_file() && before.len() <= 32 * 1024 * 1024,
        "plan must be a bounded nonsymlink regular file"
    );
    let mut file = File::open(path)?;
    let same = |after: fs::Metadata| {
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
            && after.is_file()
    };
    ensure!(same(file.metadata()?), "plan changed while opening");
    let mut bytes = Vec::new();
    (&mut file)
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == before.len()
            && same(file.metadata()?)
            && same(fs::symlink_metadata(path)?),
        "plan changed while reading"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn saved_descriptions_reject_duplicate_json_keys_at_every_level() {
        for bytes in [
            br#"{"execution_authorized_by_plan":false,"execution_authorized_by_plan":false}"#.as_slice(),
            br#"{"sources":[{"description":{"execution_authorized":false,"execution_authorized":false}}]}"#.as_slice(),
            br#"{"sources":[{"description":{"changed_fields":[{"field":"year","field":"year"}]}}]}"#.as_slice(),
        ] {
            assert!(serde_json::from_slice::<unique_json::UniqueJson>(bytes).is_err());
        }
        assert!(
            serde_json::from_slice::<unique_json::UniqueJson>(
                br#"{"sources":[{"description":{"execution_authorized":false}}]}"#
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_noncanonical_ids_and_ambiguous_selectors() {
        for value in [
            json!("ABCDEF1234567890"),
            json!("1234"),
            json!("0000000000000001"),
            json!(42),
        ] {
            assert!(pid(&value).is_err());
        }
        assert_eq!(pid(&json!("ffffffffffffffff")).unwrap(), u64::MAX);
        assert!(strings(&json!(["a", "a"])).is_err());
        assert!(strings(&json!([1])).is_err());
    }
}
