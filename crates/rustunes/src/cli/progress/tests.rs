use super::*;
use crate::engine::{Hold, SkipStage, Skipped};
use std::path::Path;

/// A sync that left out one file while planning and dropped one while
/// sending, as `outcome`.
pub(crate) fn skipping(outcome: SyncOutcome) -> SyncResult {
    let mut result = SyncResult::new(
        outcome,
        Path::new("/state/runs/1"),
        Path::new("/state/runs/1/plan.json"),
        Path::new("/state/journal"),
        2,
        0,
    );
    result.skipped = vec![
        Skipped {
            path: "Artist A - \"One\".flac".into(),
            code: "mp3_source_profile_unsupported",
            reason: "it lists 2 titles, and the phone keeps one. Edit the tag so it has a single title.".into(),
            detail: "FLAC source is outside the insert-batch profile: title has 2 values in the file's tag; edit the tag to keep one".into(),
            stage: SkipStage::Plan,
        },
        Skipped {
            path: "Artist B - Two.flac".into(),
            code: "flac_conversion_failed",
            reason: "its conversion to ALAC failed its check, so it was not sent.".into(),
            detail: "ffmpeg exited with status 1".into(),
            stage: SkipStage::Send,
        },
    ];
    result
}

#[test]
fn a_finished_sync_names_each_file_it_left_out_in_plain_words() {
    let result = skipping(SyncOutcome::Completed);
    assert!(!result.sent_all());
    let lines = finished(&result);
    let at = lines.iter().position(|l| l == "Skipped 2 files:").unwrap();
    assert_eq!(
        lines[at + 1..],
        [
            "  Artist A - \"One\".flac: it lists 2 titles, and the phone keeps one. Edit the tag so it has a single title.",
            "  Artist B - Two.flac: its conversion to ALAC failed its check, so it was not sent.",
            "The run directory (/state/runs/1) and --message-format json have the full details.",
        ]
    );
    // No sentence is a JSON string, and the full refusal stays out.
    assert!(
        lines
            .iter()
            .all(|l| !l.contains("\\\"") && !l.contains("profile"))
    );
    // A dry run would leave them out.
    let mut dry = skipping(SyncOutcome::Preflight);
    dry.skipped.truncate(1);
    assert!(dry.sent_all());
    let lines = finished(&dry);
    assert!(
        lines.contains(&"Would skip 1 file:".to_owned()),
        "{lines:?}"
    );
    // Nothing left out, nothing said.
    dry.skipped.clear();
    assert_eq!(finished(&dry).len(), 1);
}

#[test]
fn a_blocked_sync_says_what_holds_it_and_how_to_send_the_rest() {
    let holds = SyncHolds {
        holds: vec![Hold {
            path: Some("a.m4a".into()),
            code: "multiple_existing_audio_matches".into(),
            reason: "the phone has more than one track with exactly its audio, and rustunes won't choose between them.".into(),
        }],
        hint: Some("To send the rest now without them, run `rustunes push --exclude a.m4a`: push sends new tracks only, and removes and updates nothing.".into()),
        skipped: Vec::new(),
    };
    assert_eq!(
        blocked(&holds),
        "Nothing was sent. This must be fixed first:\n  a.m4a: the phone has more than one track with exactly its audio, and rustunes won't choose between them.\nTo send the rest now without them, run `rustunes push --exclude a.m4a`: push sends new tracks only, and removes and updates nothing."
    );
    let journal = SyncHolds {
        holds: vec![Hold {
            path: None,
            code: "outstanding_prepared_requires_resolution".into(),
            reason: "an earlier run prepared a change it never started.".into(),
        }],
        hint: None,
        skipped: Vec::new(),
    };
    assert_eq!(
        blocked(&journal),
        "Nothing was sent. This must be fixed first:\n  journal: an earlier run prepared a change it never started."
    );
    assert_eq!(
        blocked(&SyncHolds {
            holds: Vec::new(),
            hint: None,
            skipped: Vec::new(),
        }),
        "Nothing was sent."
    );
}

#[test]
fn an_untested_catalogue_version_names_it_and_the_tested_ones() {
    assert_eq!(
        observation_line(&Observation::UntestedCatalogueVersion(2_420_000)),
        "The phone's Music database is version 2420000, which rustunes has not been tested with (tested: 2400060, 2410000); every table and column it reads is present"
    );
}

#[test]
fn a_blocked_sync_also_names_the_files_it_would_skip() {
    let holds = SyncHolds {
        holds: vec![Hold {
            path: Some("a.m4a".into()),
            code: "multiple_existing_audio_matches".into(),
            reason: "the phone has more than one track with exactly its audio, and rustunes won't choose between them.".into(),
        }],
        hint: None,
        skipped: vec![Skipped {
            path: "big.wav".into(),
            code: "source_exceeds_transfer_limit",
            reason: "it is larger than the 64 MiB rustunes sends as one track. Encode it smaller.".into(),
            detail: "Source has 67108868 bytes; one native audio transfer takes at most 67108864 bytes".into(),
            stage: SkipStage::Plan,
        }],
    };
    assert_eq!(
        blocked(&holds),
        "Nothing was sent. This must be fixed first:\n  a.m4a: the phone has more than one track with exactly its audio, and rustunes won't choose between them.\nIt would also skip 1 file:\n  big.wav: it is larger than the 64 MiB rustunes sends as one track. Encode it smaller."
    );
}

fn ahead(done: usize) -> Event<'static> {
    Event::Observation(Observation::ReadingAhead {
        done,
        total: 3,
        bytes_done: done as u64 * 10,
        bytes_total: 30,
    })
}

fn fingerprinted(completed: usize, reused: bool) -> Event<'static> {
    Event::Observation(Observation::AudioFingerprinted {
        track: 1,
        completed,
        total: 4,
        bytes: 0,
        other_codec: false,
        reused,
    })
}

#[test]
fn on_a_terminal_reading_sources_draws_the_bar_by_bytes() {
    let event = Event::Observation(Observation::InspectingSource {
        number: 316,
        total: 1045,
        path: "a.m4a",
        file_bytes: 5,
        bytes_done: 2_100,
        bytes_total: 5_800,
    });
    assert_eq!(
        on_terminal(&event),
        Some(Terminal::Draw(bar::Bar {
            label: "Reading sources",
            done: 315,
            total: 1045,
            bytes: Some((2_100, 5_800)),
        }))
    );
    assert_eq!(
        on_terminal(&ahead(1)),
        Some(Terminal::Draw(bar::Bar {
            label: "Reading M4A sources ahead",
            done: 1,
            total: 3,
            bytes: Some((10, 30)),
        }))
    );
    // The bar replaces the line every 5 s; the last count stays as a line.
    // The first names the pass the bar shows.
    for (counted, name) in [
        (
            (|done| Event::InspectingSources { done, total: 10 }) as fn(usize) -> Event<'static>,
            "Reading selected sources",
        ),
        (
            |done| Event::Observation(Observation::RevalidatingSources { done, total: 10 }),
            "Revalidating planned sources",
        ),
    ] {
        assert_eq!(on_terminal(&counted(0)), Some(Terminal::Begin(name)));
        assert_eq!(on_terminal(&counted(5)), Some(Terminal::Skip));
        assert_eq!(on_terminal(&counted(10)), Some(Terminal::End));
    }
    for done in [
        Event::Observation(Observation::SourcesInspected {
            total: 3,
            bytes_total: 30,
        }),
        ahead(3),
    ] {
        assert_eq!(on_terminal(&done), Some(Terminal::End));
    }
}

#[test]
fn on_a_terminal_indexing_phone_audio_draws_the_bar_by_count() {
    for reused in [true, false] {
        assert_eq!(
            on_terminal(&fingerprinted(2, reused)),
            Some(Terminal::Draw(bar::Bar {
                label: "Indexing phone audio",
                done: 2,
                total: 4,
                bytes: None,
            }))
        );
        assert_eq!(on_terminal(&fingerprinted(4, reused)), Some(Terminal::End));
    }
}

#[test]
fn other_lines_print_on_a_terminal_as_on_a_pipe() {
    let warning = Event::Observation(Observation::CacheUnavailable {
        cache: "source cache",
        error: "locked".into(),
    });
    assert_eq!(on_terminal(&warning), None);
    assert_eq!(
        on_terminal(&Event::CheckingCover {
            number: 1,
            total: 2
        }),
        None
    );
}

#[test]
fn on_a_pipe_reading_ahead_prints_its_start_and_end_only() {
    assert_eq!(
        console_line(&ahead(0)).unwrap().as_deref(),
        Some("Reading 3 M4A sources ahead on several threads")
    );
    assert_eq!(console_line(&ahead(1)).unwrap(), None);
    assert_eq!(
        console_line(&ahead(3)).unwrap().as_deref(),
        Some("Read 3/3 M4A sources ahead")
    );
    // As before: every 5 s, and each copied track.
    assert_eq!(
        console_line(&Event::InspectingSources { done: 5, total: 10 })
            .unwrap()
            .as_deref(),
        Some("Inspected 5/10 selected sources")
    );
    assert_eq!(
        console_line(&fingerprinted(2, false)).unwrap().as_deref(),
        Some("Fingerprinted 2/4 tracks; 0 bytes verified")
    );
}
