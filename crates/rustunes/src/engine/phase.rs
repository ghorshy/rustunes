//! How long each phase of a run took and how much memory the process held,
//! reported to frontends as [`Event::PhaseFinished`](super::Event).
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

/// A phase of a run. Frontends choose how to name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Planning a sync or a change, from the snapshot to the plan.
    Plan,
    SelectSources,
    InspectSources,
    /// Reading every M4A's fingerprint and cover on several threads first.
    ReadM4aAhead,
    AllocateCandidates,
    /// Comparing the inspected sources with the phone and building the plan.
    Compare,
    /// Inspecting and comparing again for a plan saved earlier.
    CheckSavedPlan,
    /// Carrying out a plan, or checking it in a dry run.
    Apply,
    CaptureCatalogue,
    /// Insert batch `n` (from 1) of a sync.
    SendBatch(usize),
    /// Removal batch `n` (from 1) of a sync.
    RemoveBatch(usize),
    UpdateMetadata,
    SendCovers,
}

/// The process's resident memory when a phase ended, and the most it had
/// held by then (which only grows over a run).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Memory {
    pub rss_bytes: u64,
    pub peak_rss_bytes: u64,
}

/// One finished phase. `started` is measured from the first phase this
/// process timed, so a phase nested in another lies within its span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhaseTiming {
    pub phase: Phase,
    pub started: Duration,
    pub elapsed: Duration,
    /// None where the platform does not say.
    pub memory: Option<Memory>,
}

/// Start timing `phase`; it is reported when the returned guard is dropped,
/// however the work ends (returned, failed, or cancelled).
#[must_use = "the phase ends when this guard is dropped"]
pub(crate) fn start(phase: Phase) -> Running {
    Running {
        phase,
        started: since_first(),
    }
}

pub(crate) struct Running {
    phase: Phase,
    started: Duration,
}

impl Drop for Running {
    fn drop(&mut self) {
        let timing = PhaseTiming {
            phase: self.phase,
            started: self.started,
            elapsed: since_first().saturating_sub(self.started),
            memory: memory(),
        };
        // A report the frontend cannot take changes nothing about the phase.
        let _ = super::observe(super::Event::PhaseFinished(&timing));
    }
}

/// Phases in the order they started, each with how many of the others it lies
/// within (0 for a phase inside none), as a summary lists them.
pub fn nest(phases: &[PhaseTiming]) -> Vec<(usize, &PhaseTiming)> {
    let mut ordered: Vec<&PhaseTiming> = phases.iter().collect();
    // At one start, the longer phase holds the shorter.
    ordered.sort_by_key(|p| (p.started, std::cmp::Reverse(p.elapsed)));
    let end = |p: &PhaseTiming| p.started + p.elapsed;
    let mut open: Vec<&PhaseTiming> = Vec::new();
    let mut nested = Vec::with_capacity(ordered.len());
    for phase in ordered {
        while open.last().is_some_and(|outer| end(outer) < end(phase)) {
            open.pop();
        }
        nested.push((open.len(), phase));
        open.push(phase);
    }
    nested
}

fn since_first() -> Duration {
    static FIRST: OnceLock<Instant> = OnceLock::new();
    FIRST.get_or_init(Instant::now).elapsed()
}

/// `VmRSS` and `VmHWM` from `/proc/self/status`.
fn memory() -> Option<Memory> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.trim().strip_suffix("kB"))
            .and_then(|kib| kib.trim().parse::<u64>().ok())
            .map(|kib| kib * 1024)
    };
    Some(Memory {
        rss_bytes: field("VmRSS:")?,
        peak_rss_bytes: field("VmHWM:")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, Event};
    use std::sync::{Arc, Mutex};

    fn recording() -> (Engine, Arc<Mutex<Vec<PhaseTiming>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let engine = Engine::new(crate::engine::test_engine().config, move |event| {
            if let Event::PhaseFinished(timing) = event {
                sink.lock().unwrap().push(timing.clone());
            }
            Ok(())
        });
        (engine, seen)
    }

    #[test]
    fn a_phase_reports_its_time_and_memory_when_it_ends() {
        let (engine, seen) = recording();
        engine.with_observer(|| {
            let _phase = start(Phase::InspectSources);
            std::thread::sleep(Duration::from_millis(20));
        });
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let phase = &seen[0];
        assert_eq!(phase.phase, Phase::InspectSources);
        assert!(phase.elapsed >= Duration::from_millis(20), "{phase:?}");
        let memory = phase.memory.unwrap();
        assert!(memory.rss_bytes > 0 && memory.peak_rss_bytes >= memory.rss_bytes);
    }

    #[test]
    fn a_failed_phase_is_reported_too() {
        let (engine, seen) = recording();
        let failed: anyhow::Result<()> = engine.with_observer(|| {
            let _phase = start(Phase::Compare);
            anyhow::bail!("no plan")
        });
        assert!(failed.is_err());
        assert_eq!(seen.lock().unwrap()[0].phase, Phase::Compare);
    }

    #[test]
    fn a_phase_inside_another_nests_under_it() {
        let (engine, seen) = recording();
        engine.with_observer(|| {
            let _plan = start(Phase::Plan);
            std::thread::sleep(Duration::from_millis(5));
            {
                let _inspect = start(Phase::InspectSources);
                std::thread::sleep(Duration::from_millis(5));
            }
            let _compare = start(Phase::Compare);
        });
        let after = start(Phase::Apply);
        drop(after);
        let seen = seen.lock().unwrap();
        let order: Vec<_> = seen.iter().map(|p| p.phase).collect();
        assert_eq!(
            order,
            [Phase::InspectSources, Phase::Compare, Phase::Plan],
            "an inner phase finishes first"
        );
        let nested: Vec<_> = nest(&seen).into_iter().map(|(d, p)| (d, p.phase)).collect();
        assert_eq!(
            nested,
            [
                (0, Phase::Plan),
                (1, Phase::InspectSources),
                (1, Phase::Compare)
            ]
        );
    }

    #[test]
    fn a_phase_after_another_is_not_inside_it() {
        let at = |started, elapsed| PhaseTiming {
            phase: Phase::SendBatch(started as usize),
            started: Duration::from_millis(started),
            elapsed: Duration::from_millis(elapsed),
            memory: None,
        };
        let phases = [at(10, 5), at(0, 10), at(20, 1)];
        let depths: Vec<_> = nest(&phases).into_iter().map(|(d, _)| d).collect();
        // The one at 10 starts as the first ends: inside nothing.
        assert_eq!(depths, [0, 0, 0]);
    }

    #[test]
    fn without_an_observer_a_phase_reports_nothing() {
        drop(start(Phase::Plan));
    }
}
