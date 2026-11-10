//! One ATC link for a run.
//!
//! Opening a link after more than ten minutes makes the phone start a full
//! disk-usage refresh, and 26–40 s later it stalls the USB port. Rustunes used
//! to open a link per session, so a long sync met that stall in the middle of a
//! batch. A run instead opens one link before its first capture, waits out the
//! refresh the first open starts, and gives the same link to every session;
//! each session still has its own receipts, limits and traces.
//!
//! Between sessions the link is idle while captures, uploads and verification
//! run. If the phone closes or stalls it, the next session reopens one: a
//! request whose send failed never reached the phone, so it goes out again on
//! the new link. Nothing is Started by a lost link.
use anyhow::{Context, Result, ensure};
use music_protocol::protocol::session::{Link, Prepared, Session};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::time::{Instant, timeout_at};

use crate::engine::execution::device;
use crate::engine::execution::native_insert::{Control, create_trace};
use crate::engine::execution::removal_batch::next_directory;

/// The phone starts a full refresh when a link opens if the last one is over
/// 600 s old, then stalls the port 26–40 s in (E4 27 s, E1 28.8 s, E3 33.7 s,
/// E6 about 40 s). Waiting this long on a run's first link either sees that
/// stall or shows no refresh was due.
pub(crate) const STALL_WINDOW: Duration = Duration::from_secs(45);
/// A link that dies between its handshake and the request is replaced this
/// many times; anything else about a failed session is the batch's own.
const UNSENT_RETRIES: usize = 3;

/// A run's one ATC link, opened lazily and reopened only when it is lost.
pub(crate) struct RunLink {
    directory: PathBuf,
    link: Option<Link>,
    udid: Option<String>,
    /// The run's first link has not waited out the refresh yet.
    fresh: bool,
}

impl RunLink {
    /// A link whose traces go under `directory`, created on first use.
    pub(crate) fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            link: None,
            udid: None,
            fresh: true,
        }
    }

    /// Open the run's link if it is not open, retrying while the phone refuses
    /// new muxes after a stall. The discovery check against the captured state
    /// stays with the caller's session.
    pub(crate) async fn open(&mut self, control: &Control) -> Result<()> {
        if self.link.is_some() {
            return Ok(());
        }
        let give_up = Instant::now() + device::REOPEN_WINDOW;
        loop {
            control.check()?;
            match self.try_open(control).await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    self.link = None;
                    self.udid = None;
                    if Instant::now() + device::REOPEN_DELAY >= give_up {
                        return Err(error).context("the phone did not accept a new ATC link");
                    }
                    tokio::time::sleep(device::REOPEN_DELAY).await;
                }
            }
        }
    }

    async fn try_open(&mut self, control: &Control) -> Result<()> {
        let deadline = control
            .deadline
            .min(Instant::now() + Duration::from_secs(30));
        let provider = timeout_at(deadline, device::provider())
            .await
            .context("ATC discovery deadline")??;
        if !self.directory.try_exists()? {
            create_trace(&self.directory)?;
        }
        let directory = next_directory(&self.directory, "open")?;
        create_trace(&directory)?;
        self.link = Some(Link::connect(&provider, &directory, deadline).await?);
        self.udid = Some(provider.udid);
        Ok(())
    }

    /// Wait out the refresh the run's first link starts, then open a fresh
    /// link before the first capture or batch. Both outcomes end here: a
    /// stall closes the link, and a wait that ends in silence cannot resume
    /// the receive. Nothing is Started either way, so no batch is at risk;
    /// and because the phone has just refreshed, the new link is partial.
    pub(crate) async fn settle(&mut self, control: &Control) -> Result<()> {
        if !self.fresh {
            return Ok(());
        }
        self.fresh = false;
        // The refresh starts when this first link opens, not before.
        self.open(control).await?;
        if let Some(mut link) = self.link.take() {
            let window =
                STALL_WINDOW.min(control.deadline.saturating_duration_since(Instant::now()));
            link.wait_lost(window).await;
        }
        self.udid = None;
        self.open(control).await
    }

    /// Start one session on the run's link. A link the phone closed while
    /// idle is replaced and the same preparation goes out on it; a session
    /// that reaches `Phase::Finished` gives the link back through `keep`.
    pub(crate) async fn begin(
        &mut self,
        prepared: Prepared,
        trace: &Path,
        expected: &str,
        control: &Control,
    ) -> Result<Session> {
        let mut prepared = prepared;
        for _ in 0..UNSENT_RETRIES {
            self.open(control).await?;
            ensure!(
                self.udid.as_deref() == Some(expected),
                "the run's link is to another device"
            );
            let link = self.link.take().context("the run's link vanished")?;
            match link
                .begin_sync_or_unsent(prepared, trace, control.deadline)
                .await
            {
                Ok(session) => return Ok(session),
                Err((_error, Some(back))) => {
                    // Nothing reached the phone on that link; reopen.
                    prepared = back;
                    self.udid = None;
                }
                Err((error, None)) => return Err(error),
            }
        }
        anyhow::bail!("the phone did not accept the session request on a new link")
    }

    /// Take back a finished session's link. A session that could not hand it
    /// back leaves the next one to open a new link.
    pub(crate) fn keep(&mut self, link: Option<Link>) {
        match link {
            Some(link) => self.link = Some(link),
            None => {
                self.link = None;
                self.udid = None;
            }
        }
    }
}
