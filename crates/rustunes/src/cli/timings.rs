//! The run's phase summary: always in the log, on the console with
//! `--timings`.
use super::{logging, machine};
use crate::engine::{
    self, PhaseTiming,
    phase::{Memory, Phase},
};
use anyhow::Result;
use std::sync::Mutex;
use std::time::Duration;

static PHASES: Mutex<Vec<PhaseTiming>> = Mutex::new(Vec::new());

/// Keep a finished phase for the summary.
pub(crate) fn record(phase: &PhaseTiming) {
    PHASES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(phase.clone());
}

/// Log the summary of the phases recorded so far, and show it when `shown`:
/// on stderr, or as a `phase_timings` notice under `--message-format json`.
pub(crate) fn report(shown: bool) -> Result<()> {
    let phases = std::mem::take(&mut *PHASES.lock().unwrap_or_else(|e| e.into_inner()));
    let lines = summary(&phases);
    if lines.is_empty() {
        return Ok(());
    }
    for line in &lines {
        logging::notice(machine::Level::Info, line);
    }
    if !shown {
        return Ok(());
    }
    if machine::enabled() {
        let phases: Vec<_> = phases
            .iter()
            .map(|p| {
                serde_json::json!({
                    "phase": label(p.phase),
                    "started_ms": p.started.as_millis() as u64,
                    "elapsed_ms": p.elapsed.as_millis() as u64,
                    "rss_bytes": p.memory.map(|m| m.rss_bytes),
                    "peak_rss_bytes": p.memory.map(|m| m.peak_rss_bytes),
                })
            })
            .collect();
        machine::notice(
            "phase_timings",
            machine::Level::Info,
            lines.join("\n"),
            serde_json::json!({ "phases": phases }),
        )
    } else {
        for line in &lines {
            eprintln!("{line}");
        }
        Ok(())
    }
}

/// The summary's lines: phases in the order they started, a phase inside
/// another indented under it, each with its time, the memory held when it
/// ended and the most held so far.
pub(crate) fn summary(phases: &[PhaseTiming]) -> Vec<String> {
    if phases.is_empty() {
        return Vec::new();
    }
    let rows: Vec<_> = engine::phase::nest(phases)
        .into_iter()
        .map(|(depth, p)| (format!("{}{}", "  ".repeat(depth + 1), label(p.phase)), p))
        .collect();
    let width = rows.iter().map(|(label, _)| label.len()).max().unwrap_or(0) + 2;
    let mut lines =
        vec!["Phases (time; memory when each ended, and the most held so far):".to_owned()];
    lines.extend(rows.into_iter().map(|(label, phase)| {
        format!(
            "{label:<width$}{:>11}  {}",
            time(phase.elapsed),
            memory(phase.memory)
        )
    }));
    lines
}

fn label(phase: Phase) -> String {
    match phase {
        Phase::Plan => "plan".into(),
        Phase::SelectSources => "select sources".into(),
        Phase::InspectSources => "inspect sources".into(),
        Phase::ReadM4aAhead => "read M4A ahead".into(),
        Phase::AllocateCandidates => "allocate candidates".into(),
        Phase::Compare => "compare and plan".into(),
        Phase::CheckSavedPlan => "check the saved plan".into(),
        Phase::Apply => "apply".into(),
        Phase::CaptureCatalogue => "copy the phone's catalogue".into(),
        Phase::SendBatch(n) => format!("send batch {n}"),
        Phase::RemoveBatch(n) => format!("remove batch {n}"),
        Phase::UpdateMetadata => "update metadata".into(),
        Phase::SendCovers => "send covers".into(),
    }
}

fn time(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{:.1} s", elapsed.as_secs_f64())
    } else {
        format!("{} min {} s", seconds / 60, seconds % 60)
    }
}

fn memory(memory: Option<Memory>) -> String {
    match memory {
        Some(m) => format!(
            "{} MB, peak {} MB",
            m.rss_bytes / 1_000_000,
            m.peak_rss_bytes / 1_000_000
        ),
        None => "memory unknown".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phase(phase: Phase, started_ms: u64, elapsed_ms: u64, rss_mb: Option<u64>) -> PhaseTiming {
        PhaseTiming {
            phase,
            started: Duration::from_millis(started_ms),
            elapsed: Duration::from_millis(elapsed_ms),
            memory: rss_mb.map(|mb| Memory {
                rss_bytes: mb * 1_000_000,
                peak_rss_bytes: mb * 1_000_000 + 10_000_000,
            }),
        }
    }

    #[test]
    fn phases_are_listed_in_start_order_with_nested_ones_indented() {
        // As reported: an inner phase finishes before the one around it.
        let phases = [
            phase(Phase::ReadM4aAhead, 1_000, 130_400, Some(550)),
            phase(Phase::InspectSources, 900, 432_000, Some(560)),
            phase(Phase::SelectSources, 100, 800, Some(20)),
            phase(Phase::AllocateCandidates, 433_000, 280_000, None),
            phase(Phase::Plan, 50, 713_100, Some(570)),
            phase(Phase::SendBatch(1), 714_000, 4_240, Some(300)),
        ];
        assert_eq!(
            summary(&phases),
            [
                "Phases (time; memory when each ended, and the most held so far):",
                "  plan                   11 min 53 s  570 MB, peak 580 MB",
                "    select sources             0.8 s  20 MB, peak 30 MB",
                "    inspect sources       7 min 12 s  560 MB, peak 570 MB",
                "      read M4A ahead      2 min 10 s  550 MB, peak 560 MB",
                "    allocate candidates   4 min 40 s  memory unknown",
                "  send batch 1                 4.2 s  300 MB, peak 310 MB",
            ]
        );
    }

    #[test]
    fn a_run_without_phases_has_no_summary() {
        assert!(summary(&[]).is_empty());
    }
}
