//! What the phone says about the disk-usage refresh a new ATC link starts.
//!
//! While atc answers a new link it logs how long ago its last full refresh
//! started, then which data classes it gathers now (iOS 27.2, 2026-11-10):
//!
//! ```text
//! atc(AirTrafficDevice)[53] <Notice>: Sending disk usage. lastUpdate=1156.07s ago
//! atc(AirTrafficDevice)[53] <Notice>: <ATDeviceDiskUsageProvider: 0x…> Gathering updated usage metrics for data classes Photo,MessagePart,Book,UserData,Keybag,Logs,File,Ringtone,Media,Application,AirFair2
//! ```
//!
//! A full refresh (every class) is the one that stalls a USB port 16–40 s
//! after it starts; a partial one (`Media` alone) never did. `lastUpdate`
//! counts from the start of the last full refresh: a link opened 31.91 s
//! after one logged `lastUpdate=31.91s ago`, and that earlier refresh still
//! stalled the port 3 s later. So a partial refresh leaves only what is left
//! of the last full one's window to wait.
//!
//! The gathering line does not always come: on 2026-11-16 a link opened 223 s
//! after a full refresh got only the disk-usage line within five seconds, and
//! the port did not stall. The phone starts a full refresh only when the last
//! one is over 600 s old, so a younger `lastUpdate` alone names a partial one.
use std::time::Duration;

use anyhow::{Context, Result};
use idevice::{IdeviceService, services::syslog_relay::SyslogRelayClient};
use tokio::time::{Instant, timeout_at};

use crate::engine::execution::{device, native_insert::Control};

/// How long the relay may take to start. Without it the link waits the whole
/// window, as it would without this signal.
const SYSLOG_START: Duration = Duration::from_secs(5);
/// How long after the link opened atc's two lines may take to be read. They
/// come during the handshake, so the relay holds them already.
const VERDICT_WINDOW: Duration = Duration::from_secs(5);
/// Below this `lastUpdate` the refresh is partial even when no gathering line
/// says so: the phone's threshold is 600 s, less a margin.
const PARTIAL_BELOW: Duration = Duration::from_secs(540);

/// The refresh a link's opening started.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Refresh {
    /// Every data class: the stall it brings is still to come.
    Full,
    /// `Media` alone. The last full refresh started `since_full` ago.
    Partial { since_full: Duration },
}

/// atc's lines in the order they come, naming the refresh once a gathering
/// line follows a disk-usage line.
#[derive(Debug, Default)]
pub(crate) struct Verdict {
    since_full: Option<Duration>,
    /// The lines the verdict rests on, kept as evidence.
    pub(crate) lines: Vec<String>,
}

impl Verdict {
    pub(crate) fn feed(&mut self, line: &str) -> Option<Refresh> {
        if !from_atc(line) {
            return None;
        }
        if let Some(since) = last_update(line) {
            self.since_full = Some(since);
            self.lines.push(line.to_owned());
            return None;
        }
        let since_full = self.since_full?;
        let classes = gathered_classes(line)?;
        self.lines.push(line.to_owned());
        Some(if classes == ["Media"] {
            Refresh::Partial { since_full }
        } else {
            Refresh::Full
        })
    }

    /// The refresh once no gathering line came: partial when the last full
    /// one is young enough that the phone starts none, else unknown.
    pub(crate) fn without_gathering(&self) -> Option<Refresh> {
        self.since_full
            .filter(|since_full| *since_full < PARTIAL_BELOW)
            .map(|since_full| Refresh::Partial { since_full })
    }
}

/// Whether the line's process is atc: the word before its level, as in
/// `Nov 10 20:44:49.594428 atc(AirTrafficDevice)[53] <Notice>: …`, with or
/// without the device's name before it.
fn from_atc(line: &str) -> bool {
    line.split_once(" <")
        .and_then(|(head, _)| head.split_whitespace().last())
        .is_some_and(|process| process.starts_with("atc(") || process.starts_with("atc["))
}

/// `lastUpdate` of a `Sending disk usage` line.
fn last_update(line: &str) -> Option<Duration> {
    let (_, rest) = line.split_once("Sending disk usage. lastUpdate=")?;
    let seconds: f64 = rest.strip_suffix("s ago")?.trim().parse().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds))
}

/// The classes of a `Gathering updated usage metrics` line.
fn gathered_classes(line: &str) -> Option<Vec<&str>> {
    let (_, rest) = line.split_once("Gathering updated usage metrics for data classes ")?;
    let classes: Vec<&str> = rest.trim().split(',').map(str::trim).collect();
    (!classes.iter().any(|class| class.is_empty())).then_some(classes)
}

/// How much of `window` a new link must still wait for a stall: all of it
/// after a full refresh or when atc said nothing, what is left of the last
/// full refresh's window after a partial one.
pub(crate) fn still_due(refresh: Option<Refresh>, window: Duration) -> Duration {
    match refresh {
        Some(Refresh::Partial { since_full }) => window.saturating_sub(since_full),
        Some(Refresh::Full) | None => window,
    }
}

/// The phone's system log, started before a link opens so that it holds
/// what atc says while answering it.
pub(crate) struct Syslog(SyslogRelayClient);

impl Syslog {
    /// The relay, or why it could not start; the caller then waits the whole
    /// window.
    pub(crate) async fn start(control: &Control) -> Result<Self> {
        let deadline = control.deadline.min(Instant::now() + SYSLOG_START);
        let provider = timeout_at(deadline, device::provider())
            .await
            .context("device discovery for the system log timed out")??;
        let client = timeout_at(deadline, SyslogRelayClient::connect(&provider))
            .await
            .context("starting the system log relay timed out")?
            .context("starting the system log relay")?;
        Ok(Self(client))
    }

    /// Read what atc said about the link that just opened, at most
    /// `VERDICT_WINDOW`. Without a gathering line by then, a young
    /// `lastUpdate` still names a partial refresh; None when atc said nothing
    /// that decides it.
    pub(crate) async fn verdict(mut self, control: &Control) -> (Option<Refresh>, Verdict) {
        let deadline = control.deadline.min(Instant::now() + VERDICT_WINDOW);
        let mut verdict = Verdict::default();
        while let Ok(Ok(line)) = timeout_at(deadline, self.0.next()).await {
            if let Some(refresh) = verdict.feed(&line) {
                return (Some(refresh), verdict);
            }
        }
        (verdict.without_gathering(), verdict)
    }
}

#[cfg(test)]
mod tests;
