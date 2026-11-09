//! One progress line on a terminal's stderr, redrawn in place. Whatever
//! prints another line clears it first ([`clear`]), and so does Ctrl-C.
use std::{
    io::{IsTerminal, Write},
    sync::{
        Mutex, Once, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// The line is on the screen.
static DRAWN: AtomicBool = AtomicBool::new(false);
/// When it was last drawn: redraws closer together are skipped.
static LAST: Mutex<Option<Instant>> = Mutex::new(None);
const REDRAW: Duration = Duration::from_millis(100);
/// The name of the source pass under way, which its bar shows in place of
/// its own label.
static PASS: Mutex<Option<&'static str>> = Mutex::new(None);

/// What the line shows: `done` of `total` items and, where the step reads
/// bytes, `(done, total)` of them, which then fill the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Bar {
    pub label: &'static str,
    pub done: usize,
    pub total: usize,
    pub bytes: Option<(u64, u64)>,
}

/// Whether stderr is a terminal, where the bar replaces progress lines.
pub(super) fn on_a_terminal() -> bool {
    static TERMINAL: OnceLock<bool> = OnceLock::new();
    *TERMINAL.get_or_init(|| std::io::stderr().is_terminal())
}

/// Draw `bar` over the line drawn before, at most every [`REDRAW`].
pub(super) fn draw(bar: &Bar) {
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if DRAWN.load(Ordering::Relaxed) && last.is_some_and(|at| at.elapsed() < REDRAW) {
        return;
    }
    static INTERRUPT: Once = Once::new();
    INTERRUPT.call_once(clear_on_interrupt);
    let pass = *PASS.lock().unwrap_or_else(|e| e.into_inner());
    let bar = Bar {
        label: pass.filter(|_| bar.label == SOURCES).unwrap_or(bar.label),
        ..*bar
    };
    let line = render(&bar, width().unwrap_or(80));
    let mut stderr = std::io::stderr().lock();
    // A terminal that takes no more output has nothing to show it on.
    let _ = write!(stderr, "\r{line}\x1b[K").and_then(|()| stderr.flush());
    DRAWN.store(true, Ordering::Relaxed);
    *last = Some(Instant::now());
}

/// The label of a bar over sources that no pass has named.
pub(super) const SOURCES: &str = "Reading sources";

/// Name the source pass that begins, or forget it once it ends.
pub(super) fn name_pass(name: Option<&'static str>) {
    *PASS.lock().unwrap_or_else(|e| e.into_inner()) = name;
}

/// Take the line off the screen, if it is there, so the next line prints
/// from the start of an empty one.
pub(crate) fn clear() {
    if DRAWN.swap(false, Ordering::Relaxed) {
        let mut stderr = std::io::stderr().lock();
        let _ = write!(stderr, "\r\x1b[K").and_then(|()| stderr.flush());
    }
}

/// The terminal's columns.
fn width() -> Option<usize> {
    // SAFETY: TIOCGWINSZ writes a `winsize` into the one passed.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let read = unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut size) };
    (read == 0 && size.ws_col > 0).then_some(usize::from(size.ws_col))
}

/// On Ctrl-C, clear the line, then end as the signal would have.
fn clear_on_interrupt() {
    extern "C" fn interrupted(signal: libc::c_int) {
        const CLEAR: &[u8] = b"\r\x1b[K";
        // SAFETY: only async-signal-safe calls: write, signal, raise.
        unsafe {
            if DRAWN.load(Ordering::Relaxed) {
                libc::write(libc::STDERR_FILENO, CLEAR.as_ptr().cast(), CLEAR.len());
            }
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
    }
    // SAFETY: the handler above only makes async-signal-safe calls.
    unsafe {
        libc::signal(
            libc::SIGINT,
            interrupted as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
}

/// The line for `bar`, no wider than `width - 1` characters so the cursor
/// stays on it.
pub(super) fn render(bar: &Bar, width: usize) -> String {
    const CELLS: usize = 20;
    let filled = match bar.bytes {
        Some((done, total)) if total > 0 => cells(done as u128, total as u128, CELLS),
        _ => cells(bar.done as u128, bar.total as u128, CELLS),
    };
    let mut line = format!(
        "[{}{}] {}/{}",
        "#".repeat(filled),
        "-".repeat(CELLS - filled),
        bar.done,
        bar.total
    );
    if let Some((done, total)) = bar.bytes {
        let (unit, name) = match total {
            1_000_000_000.. => (1e9, "GB"),
            1_000_000.. => (1e6, "MB"),
            _ => (1e3, "kB"),
        };
        line.push_str(&format!(
            " · {:.1}/{:.1} {name}",
            done as f64 / unit,
            total as f64 / unit
        ));
    }
    line.push_str("  ");
    line.push_str(bar.label);
    line.chars().take(width.saturating_sub(1)).collect()
}

/// How many of `cells` the share `done / total` fills, at most all of them.
fn cells(done: u128, total: u128, cells: usize) -> usize {
    if total == 0 {
        return 0;
    }
    (done.min(total) * cells as u128 / total) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(done: usize, bytes: Option<(u64, u64)>) -> Bar {
        Bar {
            label: "Reading sources",
            done,
            total: 1045,
            bytes,
        }
    }

    #[test]
    fn the_bar_fills_by_bytes_with_files_and_bytes_beside_it() {
        let bar = reading(315, Some((2_100_000_000, 5_800_000_000)));
        assert_eq!(
            render(&bar, 100),
            "[#######-------------] 315/1045 · 2.1/5.8 GB  Reading sources"
        );
    }

    #[test]
    fn without_bytes_the_bar_fills_by_count() {
        let bar = Bar {
            label: "Indexing phone audio",
            done: 3,
            total: 4,
            bytes: None,
        };
        assert_eq!(
            render(&bar, 100),
            "[###############-----] 3/4  Indexing phone audio"
        );
    }

    #[test]
    fn smaller_totals_are_shown_in_megabytes_or_kilobytes() {
        let bar = reading(1, Some((260_000, 52_400_000)));
        assert!(render(&bar, 100).contains(" · 0.3/52.4 MB  "));
        let bar = reading(1, Some((100, 2_000)));
        assert!(render(&bar, 100).contains(" · 0.1/2.0 kB  "));
    }

    #[test]
    fn a_narrow_terminal_cuts_the_end_of_the_line() {
        let bar = reading(315, Some((2_100_000_000, 5_800_000_000)));
        assert_eq!(render(&bar, 40), "[#######-------------] 315/1045 · 2.1/5");
        assert_eq!(render(&bar, 40).chars().count(), 39);
    }
}
