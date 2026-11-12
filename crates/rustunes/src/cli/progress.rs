//! Terminal rendering for typed engine observations.
use super::bar;
use crate::engine::{
    ApplyResult, CoverResult, Event, Observation, Skipped, SyncHolds, SyncOutcome, SyncResult,
    execution::device,
};
use anyhow::Result;

/// Terminal output for an engine event. `configured_engine_output` sends
/// `--message-format json` output and deletion events elsewhere first.
pub(crate) fn write(event: Event<'_>) -> Result<()> {
    if bar::on_a_terminal() {
        match on_terminal(&event) {
            Some(Terminal::Draw(progress)) => {
                bar::draw(&progress);
                return Ok(());
            }
            Some(Terminal::Begin(label)) => {
                bar::name_pass(Some(label));
                return Ok(());
            }
            Some(Terminal::Skip) => return Ok(()),
            Some(Terminal::End) => {
                bar::clear();
                if matches!(
                    event,
                    Event::Observation(Observation::SourcesInspected { .. })
                ) {
                    bar::name_pass(None);
                }
            }
            None => {}
        }
    }
    if let Some(line) = console_line(&event)? {
        bar::clear();
        eprintln!("{line}");
        return Ok(());
    }
    match event {
        Event::PlanReady(result) => {
            bar::clear();
            super::render_plan(result, false, result.add_only)?
        }
        Event::SyncFinished(result) => {
            bar::clear();
            for line in finished(result) {
                println!("{line}");
            }
        }
        Event::ApplyFinished(result) => {
            bar::clear();
            println!("{}", applied(&result))
        }
        Event::CoverFinished(result) => {
            bar::clear();
            println!("{}", covered(&result))
        }
        _ => {}
    }
    Ok(())
}

/// What a progress event does on a terminal instead of printing its line;
/// None for an event that prints as it would on a pipe.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Terminal {
    /// Draw the bar in place.
    Draw(bar::Bar),
    /// A pass over the sources begins: its bar takes this name.
    Begin(&'static str),
    /// Print nothing: the bar shows it.
    Skip,
    /// The step the bar showed ended: clear it, then print as on a pipe.
    End,
}

pub(super) fn on_terminal(event: &Event<'_>) -> Option<Terminal> {
    let bytes_bar = |label, done, total, bytes_done, bytes_total| {
        Terminal::Draw(bar::Bar {
            label,
            done,
            total,
            bytes: Some((bytes_done, bytes_total)),
        })
    };
    Some(match *event {
        Event::Observation(Observation::InspectingSource {
            number,
            total,
            bytes_done,
            bytes_total,
            ..
        }) => bytes_bar(bar::SOURCES, number - 1, total, bytes_done, bytes_total),
        Event::Observation(Observation::ReadingAhead {
            done,
            total,
            bytes_done,
            bytes_total,
        }) if done < total => bytes_bar(
            "Reading M4A sources ahead",
            done,
            total,
            bytes_done,
            bytes_total,
        ),
        Event::Observation(Observation::AudioFingerprinted {
            completed, total, ..
        }) if completed < total => Terminal::Draw(bar::Bar {
            label: "Indexing phone audio",
            done: completed,
            total,
            bytes: None,
        }),
        Event::InspectingSources { done: 0, total } if total > 0 => {
            Terminal::Begin("Reading selected sources")
        }
        Event::Observation(Observation::RevalidatingSources { done: 0, total }) if total > 0 => {
            Terminal::Begin("Revalidating planned sources")
        }
        Event::InspectingSources { done, total }
        | Event::Observation(Observation::RevalidatingSources { done, total })
            if done < total =>
        {
            Terminal::Skip
        }
        Event::InspectingSources { .. }
        | Event::Observation(
            Observation::RevalidatingSources { .. }
            | Observation::ReadingAhead { .. }
            | Observation::AudioFingerprinted { .. }
            | Observation::SourcesInspected { .. },
        ) => Terminal::End,
        _ => return None,
    })
}

/// The line an event prints on stderr where no bar is drawn, if any.
pub(super) fn console_line(event: &Event<'_>) -> Result<Option<String>> {
    if thinned(event) {
        return Ok(None);
    }
    // Progress only `--message-format json` reports.
    if let Event::Observation(
        Observation::CapturingCatalogue
        | Observation::CopyingTrackAudio { .. }
        | Observation::InspectingSource { .. }
        | Observation::SourcesInspected { .. },
    ) = event
    {
        return Ok(None);
    }
    if ahead_in_between(event) {
        return Ok(None);
    }
    line(event)
}

/// A read-ahead's report between its first and its last.
pub(super) fn ahead_in_between(event: &Event<'_>) -> bool {
    matches!(
        event,
        Event::Observation(Observation::ReadingAhead { done, total, .. })
            if *done != 0 && done != total
    )
}

/// A cached pass's per-track line that is not printed: one line per track
/// would flood the terminal and the log.
pub(super) fn thinned(event: &Event<'_>) -> bool {
    matches!(
        event,
        Event::Observation(Observation::AudioFingerprinted {
            completed,
            total,
            reused: true,
            ..
        }) if completed % 50 != 0 && completed != total
    )
}

/// The progress line an event prints on stderr, if it prints one there.
fn transport_name(transport: device::Transport) -> &'static str {
    match transport {
        device::Transport::Usb => "USB",
        device::Transport::Wifi(_) => "Wi-Fi",
    }
}
pub(super) fn line(event: &Event<'_>) -> Result<Option<String>> {
    Ok(Some(match event {
        Event::VerificationStarting => {
            "Capturing current device state before read-only verification".into()
        }
        Event::Observation(observation) => observation_line(observation),
        Event::Device(device::Event::DeviceDetected(transport)) => match transport {
            device::Transport::Usb => "USB device detected (identifier omitted)".into(),
            device::Transport::Wifi(address) => {
                format!("Wi-Fi device detected at {address} (identifier omitted)")
            }
        },
        Event::Device(device::Event::TransportChanged { from, to, batch }) => {
            let mut line = format!("Transport: {} → {}", transport_name(*from), transport_name(*to));
            if let Some(batch) = batch {
                line.push_str(&format!(" (batch {batch})"));
            }
            line
        }
        Event::Device(device::Event::PairingRecordAvailable) => {
            "Existing pairing record available".into()
        }
        Event::RunEvidence(path) => format!("Run evidence: {}", path.display()),
        Event::ReadingDeviceSnapshot => {
            "Reading device catalogue and audio identities for an exact comparison".into()
        }
        Event::ReadingSavedSnapshot => "Planning from the device snapshot".into(),
        Event::ReadingCurrentState => {
            "Reading current device state to reconcile the unfinished operation".into()
        }
        Event::InspectingSources { done, total } => {
            format!("Inspected {done}/{total} selected sources")
        }
        Event::ComparingSources => {
            "Comparing audio identities, metadata, artwork and destination reservations...".into()
        }
        Event::PlanSaved(path) => format!("Plan saved to {}", path.display()),
        Event::SyncBlocked(holds) => blocked(holds),
        Event::CheckingCover { number, total } => format!("Checking cover {number}/{total}"),
        Event::AdditionStarted {
            number,
            total,
            source,
        } => format!("Adding {number}/{total}: {source:?}"),
        Event::AudioInsertionVerified => {
            "Verified audio and catalogue; embedded cover upload remains a separate step.".into()
        }
        Event::SyncContinuing { remaining } => format!(
            "Batched additions done; planning again for {remaining} remaining action(s), one per session."
        ),
        Event::SyncResuming(reason) => {
            format!("{reason}; planning again from a fresh capture for the rest.")
        }
        Event::FlacRefused { source, reason } => format!("Not sending {source:?}: {reason}"),
        Event::ConvertingFlacs { directory } => format!(
            "Converting FLACs to ALAC with FFmpeg in {}, a few at a time, ahead of their batches.",
            directory.display()
        ),
        Event::CoverLeftOut(source) => format!(
            "Leaving out the cover of {source:?}: its album on the phone shows another image, which push leaves as it is."
        ),
        Event::InsertBatchVerified {
            inserted,
            total,
            removed: 0,
            cleared: 0,
        } => {
            format!("Verified insert batch: {inserted}/{total} new tracks on the phone.")
        }
        Event::InsertBatchVerified {
            inserted,
            total,
            removed,
            cleared,
        } => {
            let mut message = if *removed == 0 {
                format!("Verified insert batch: {inserted}/{total} new tracks on the phone.")
            } else {
                format!(
                    "Verified insert batch: its session removed {removed} track(s), and {inserted}/{total} new tracks are on the phone."
                )
            };
            if *cleared > 0 {
                message.push_str(&format!(
                    " {cleared} of them sent no cover, so the cover the phone kept for the reused item id was cleared."
                ));
            }
            message
        }
        Event::UpdatesScheduled { tracks, sessions } => format!(
            "Metadata updates of {tracks} track(s) fit in {sessions} metadata session(s); each is planned again from a fresh capture."
        ),
        Event::UpdatesRefused(reason) => {
            format!("Metadata updates refused before any phone write:\n{reason}")
        }
        Event::MetadataBatchVerified { session, sessions } => {
            format!("Verified metadata session {session}/{sessions}.")
        }
        Event::ReplacementPhase {
            coordination,
            phase,
        } => format!("Replacement {coordination}: {phase}."),
        Event::ReplacementRemoved { removed, total } => {
            format!("Verified removal batch: {removed}/{total} tracks removed.")
        }
        Event::RefreshingBeforeSession => "Read-only catalogue drift before session; refreshing once with the same insertion intent.".into(),
        Event::ArtworkRecentCountResets { plays, skips } => {
            if !skips.is_empty() {
                format!(
                    "Artwork import may reset both recent-play and recent-skip counts to zero across the whole library; lifetime counts, dates and every other statistic must remain unchanged. Recent plays: {}; recent skips: {}",
                    serde_json::to_string(plays)?,
                    serde_json::to_string(skips)?
                )
            } else if !plays.is_empty() {
                format!(
                    "Artwork import may clear recent-play counters (lifetime counts and other statistics must remain unchanged): {}",
                    serde_json::to_string(plays)?
                )
            } else {
                return Ok(None);
            }
        }
        Event::ArtworkStarted { track_id } => {
            format!("Updating artwork for {track_id:016x}; preserving its verified audio file")
        }
        Event::JournalCarried {
            earlier,
            journal,
            carried,
            matched,
            unbound,
            gone,
            covers_proven,
            covers_unproven,
            unfinished,
        } => {
            let mut text = format!(
                "Carried the journal forward from {}: {carried} tracks carried, {matched} matched to library files, {unbound} left unbound, {gone} no longer on the phone",
                earlier.display()
            );
            if let Some(covers) = super::pull::covers_text(*covers_proven, *covers_unproven) {
                text.push('\n');
                text.push_str(&covers);
            }
            if !unfinished.is_empty() {
                text.push_str(&format!(
                    "\nThe earlier journal had {}, which this version cannot resume; the new journal holds what the phone shows now",
                    unfinished.join(" and ")
                ));
            }
            text.push_str(&format!("\nJournal: {}", journal.display()));
            text
        }
        Event::PullingTrack { number, title, .. } => {
            format!("Copying phone track {number}: {title}")
        }
        // Byte counts and starts that only `--message-format json` reports.
        Event::Uploading { .. }
        | Event::TracksPulled { .. }
        | Event::InsertBatchStarted { .. }
        | Event::RemovalBatchStarted { .. } => return Ok(None),
        // Results print on stdout, and the other renderers own their lines.
        Event::Deletion(_)
        | Event::PlanReady(_)
        | Event::SyncFinished(_)
        | Event::ApplyFinished(_)
        | Event::CoverFinished(_) => return Ok(None),
        // The run's summary reports phases together, once it ends.
        Event::PhaseFinished(_) => return Ok(None),
    }))
}

/// What a sync that sent nothing says: what holds it, and what to do.
pub(super) fn blocked(holds: &SyncHolds) -> String {
    let mut text = String::from("Nothing was sent.");
    if !holds.holds.is_empty() {
        text.push_str(&format!(
            " {} must be fixed first:",
            match holds.holds.len() {
                1 => "This".to_owned(),
                n => format!("These {n}"),
            }
        ));
        for hold in &holds.holds {
            text.push_str(&format!(
                "\n  {}: {}",
                hold.path.as_deref().unwrap_or("journal"),
                hold.reason
            ));
        }
    }
    if let Some(hint) = &holds.hint {
        text.push('\n');
        text.push_str(hint);
    }
    if !holds.skipped.is_empty() {
        for line in skipped_lines("It would also skip", &holds.skipped) {
            text.push('\n');
            text.push_str(&line);
        }
    }
    text
}

/// `verb` and how many of `skipped`, then each with why.
fn skipped_lines(verb: &str, skipped: &[Skipped]) -> Vec<String> {
    let mut lines = vec![format!(
        "{verb} {}:",
        match skipped.len() {
            1 => "1 file".to_owned(),
            n => format!("{n} files"),
        }
    )];
    lines.extend(
        skipped
            .iter()
            .map(|skipped| format!("  {}: {}", skipped.path, skipped.reason)),
    );
    lines
}

/// Each source the sync left out, with why and what to do; `verb` says
/// whether it was or would be.
fn skipped(lines: &mut Vec<String>, result: &SyncResult, verb: &str) {
    if result.skipped.is_empty() {
        return;
    }
    lines.extend(skipped_lines(verb, &result.skipped));
    lines.push(format!(
        "The run directory ({}) and --message-format json have the full details.",
        result.run.display()
    ));
}

/// What a finished sync prints on stdout.
pub(super) fn finished(result: &SyncResult) -> Vec<String> {
    let mut lines = Vec::new();
    match result.outcome {
        SyncOutcome::Blocked => {}
        SyncOutcome::Preflight => {
            lines.push(format!(
                "Dry run complete. Saved plan: {}",
                result.plan.display()
            ));
            skipped(&mut lines, result, "Would skip");
        }
        SyncOutcome::Completed => {
            lines.push(format!(
                "Sync complete. Verified {} managed covers. Preserved {} existing covers without claiming image verification.",
                result.managed_covers_verified, result.existing_unmanaged_covers_unverified
            ));
            lines.push(format!("Journal: {}", result.journal.display()));
            if result.metadata_updates_verified > 0 {
                lines.push(format!(
                    "Verified {} metadata updates.",
                    result.metadata_updates_verified
                ));
            }
            if result.covers_left_out > 0 {
                lines.push(format!(
                    "Left out {} new covers whose album shows another image; their tracks were added without them.",
                    result.covers_left_out
                ));
            }
            skipped(&mut lines, result, "Skipped");
        }
    }
    lines
}

/// What a finished apply prints on stdout.
pub(super) fn applied(result: &ApplyResult) -> String {
    match *result {
        ApplyResult::Preflight { additions } => format!(
            "Live preflight passed: {additions} additions; no phone writes or journal reservations."
        ),
        ApplyResult::PreflightSourceChanges {
            additions,
            metadata_updates,
        } => format!(
            "Live preflight passed: {additions} additions and {metadata_updates} metadata updates; no phone writes or journal reservations."
        ),
        ApplyResult::Unchanged => {
            "Current library matches the plan: no audio additions. Artwork statuses remain separate."
                .into()
        }
        ApplyResult::Completed { additions } => format!(
            "Verified {additions} audio additions. No tracks deleted. Artwork updates remain separate."
        ),
    }
}

/// What a finished cover update prints on stdout.
pub(super) fn covered(result: &CoverResult) -> String {
    match *result {
        CoverResult::Unchanged => {
            "Artwork already matches the permanent device image; no update needed.".into()
        }
        CoverResult::Planned {
            track_id,
            cache_id,
            bytes,
        } => format!(
            "Artwork plan: track {track_id:016x}, cache {cache_id}, {bytes} cover bytes; no phone writes or reservations."
        ),
        CoverResult::Verified => {
            "Artwork verified in the permanent device cache. Audio and catalogue tracks preserved."
                .into()
        }
    }
}

pub(super) fn observation_line(event: &Observation<'_>) -> String {
    match event {
        Observation::CapturingCatalogue => "Copying the phone's Music catalogue".into(),
        Observation::SourcesInspected { total, .. } => format!("Inspected {total} sources"),
        Observation::ReadingAhead { done: 0, total, .. } => {
            format!("Reading {total} M4A sources ahead on several threads")
        }
        Observation::ReadingAhead { done, total, .. } => {
            format!("Read {done}/{total} M4A sources ahead")
        }
        Observation::RevalidatingSources { done, total } => {
            format!("Revalidated {done}/{total} planned sources")
        }
        Observation::CatalogueCaptureRetained(path) => {
            format!("Catalog capture retained at {}", path.display())
        }
        Observation::UntestedCatalogueVersion(version) => {
            let tested = music_protocol::catalogue::sqlite::TESTED_USER_VERSIONS.iter();
            let tested: Vec<_> = tested.map(i64::to_string).collect();
            format!(
                "The phone's Music database is version {version}, which rustunes has not been tested with (tested: {}); every table and column it reads is present",
                tested.join(", ")
            )
        }
        Observation::RecapturingBeforeAudioIndex => "Catalogue changed before audio indexing; retaining failed attempt and recapturing once within the original deadline".into(),
        Observation::RecapturingAfterAudioIndex => "Catalogue changed during audio indexing; retaining failed attempt and recapturing once within the original deadline".into(),
        Observation::FreshCatalogue { tracks, bytes } => {
            format!("Fresh catalog accepted: {tracks} tracks, {bytes} bytes of audio to index")
        }
        Observation::AudioFingerprinted {
            completed,
            total,
            bytes,
            other_codec,
            reused,
            ..
        } => {
            if *reused {
                format!("Matched {completed}/{total} unchanged tracks to cached fingerprints")
            } else if *other_codec {
                format!("Classified other audio {completed}/{total} tracks; {bytes} bytes verified")
            } else {
                format!("Fingerprinted {completed}/{total} tracks; {bytes} bytes verified")
            }
        }
        Observation::AudioIndexRetained { tracks, bytes } => {
            format!("Complete read-only index retained: {tracks} tracks, {bytes} bytes")
        }
        Observation::StateCaptureRetained(path) => {
            format!("Complete state capture retained at {}", path.display())
        }
        Observation::RecapturingAfterDroppedConnection => "The phone dropped a connection during the read-only state capture; retaining attempt 1 and recapturing complete state once within the original deadline".into(),
        Observation::RecapturingState => "Catalog changed during its read-only capture; retaining attempt 1 and recapturing complete state once within the original deadline".into(),
        Observation::VerifyingAudioBatch {
            number,
            total,
            tracks,
            bytes,
        } => format!("Verifying audio batch {number}/{total}: {tracks} tracks, {bytes} bytes"),
        Observation::AudioBatchChecked {
            number,
            total,
            matched,
            findings,
        } => format!(
            "Audio batch {number}/{total} checked: {matched} matching files, {findings} findings"
        ),
        Observation::VerifyingArtwork { number, total } => {
            format!("Verifying track artwork {number}/{total}")
        }
        Observation::ReusedArtworkProofs { reused, total } => {
            format!("Matched {reused}/{total} unchanged covers to verified proofs")
        }
        Observation::EmptyRendition { track, edge } => format!(
            "The cover of track {track:016x} is exact on the phone, but the phone left its {edge}x{edge} rendition empty"
        ),
        Observation::CopyingTrackAudio { title, .. } => format!("Copying the audio of {title:?}"),
        Observation::InspectingSource { number, total, path, .. } => {
            format!("Inspecting source {number}/{total}: {path:?}")
        }
        Observation::CacheUnavailable { cache, error } => {
            format!("{cache} unavailable, continuing without it: {error}")
        }
    }
}

#[cfg(test)]
pub(super) mod tests;
