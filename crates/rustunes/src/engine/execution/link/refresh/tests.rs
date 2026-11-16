use super::*;

const FULL: &str = "Nov 10 20:44:49.594555 atc(AirTrafficDevice)[53] <Notice>: <ATDeviceDiskUsageProvider: 0x7484c40ac0> Gathering updated usage metrics for data classes Photo,MessagePart,Book,UserData,Keybag,Logs,File,Ringtone,Media,Application,AirFair2";
const MEDIA: &str = "Nov 10 20:47:42.887428 atc(AirTrafficDevice)[53] <Notice>: <ATDeviceDiskUsageProvider: 0x7484c40ac0> Gathering updated usage metrics for data classes Media";

fn sending(seconds: &str) -> String {
    format!(
        "Nov 10 20:44:49.594428 atc(AirTrafficDevice)[53] <Notice>: Sending disk usage. lastUpdate={seconds}s ago"
    )
}

fn verdict(lines: &[&str]) -> Option<Refresh> {
    let mut verdict = Verdict::default();
    lines.iter().find_map(|line| verdict.feed(line))
}

/// The lines of the links opened on 2026-11-10: 20:44:49 (full), 20:47:42
/// (partial, 173 s after it) and 19:02:52 (partial, 31.91 s after a full one
/// that stalled the port 3 s later).
#[test]
fn atc_names_a_full_or_a_partial_refresh_and_when_the_last_full_one_started() {
    assert_eq!(verdict(&[&sending("1156.07"), FULL]), Some(Refresh::Full));
    assert_eq!(
        verdict(&[&sending("173.12"), MEDIA]),
        Some(Refresh::Partial {
            since_full: Duration::from_secs_f64(173.12)
        })
    );
    assert_eq!(
        verdict(&[&sending("31.91"), MEDIA]),
        Some(Refresh::Partial {
            since_full: Duration::from_secs_f64(31.91)
        })
    );
}

#[test]
fn other_lines_and_a_gathering_line_without_its_disk_usage_line_name_nothing() {
    let unrelated = "Nov 10 20:44:49.600000 installd[41] <Notice>: fetchDiskUsageForIdentifiers";
    assert_eq!(verdict(&[unrelated, MEDIA]), None);
    // Another process quoting atc's words is not atc.
    let quoted = "Nov 10 20:44:49.600000 Music(MusicKit)[7] <Notice>: Sending disk usage. lastUpdate=500.00s ago";
    assert_eq!(verdict(&[quoted, MEDIA]), None);
    assert_eq!(verdict(&[&sending("173.12")]), None);
    for malformed in ["", "-1", "NaN", "inf", "12.0x"] {
        assert_eq!(
            verdict(&[&sending(malformed), MEDIA]),
            None,
            "{malformed:?}"
        );
    }
    let empty_class = MEDIA.replace("Media", "Media,");
    assert_eq!(verdict(&[&sending("173.12"), &empty_class]), None);
}

/// A link that retried its handshake may see several disk-usage lines; the
/// gathering line belongs to the last one before it.
#[test]
fn the_gathering_line_follows_the_latest_disk_usage_line() {
    assert_eq!(
        verdict(&[&sending("900.00"), &sending("12.50"), MEDIA]),
        Some(Refresh::Partial {
            since_full: Duration::from_secs_f64(12.5)
        })
    );
    let mut lines = Verdict::default();
    assert_eq!(lines.feed(&sending("1156.07")), None);
    assert_eq!(lines.feed(FULL), Some(Refresh::Full));
    assert_eq!(lines.lines, [sending("1156.07"), FULL.to_owned()]);
}

#[test]
fn a_partial_refresh_waits_only_what_is_left_of_the_last_full_ones_window() {
    let window = Duration::from_secs(45);
    assert_eq!(still_due(None, window), window);
    assert_eq!(still_due(Some(Refresh::Full), window), window);
    let partial = |seconds: f64| Refresh::Partial {
        since_full: Duration::from_secs_f64(seconds),
    };
    assert_eq!(
        still_due(Some(partial(31.91)), window),
        window - Duration::from_secs_f64(31.91)
    );
    assert_eq!(still_due(Some(partial(173.12)), window), Duration::ZERO);
    assert_eq!(still_due(Some(partial(45.0)), window), Duration::ZERO);
}

/// The relay may name the device before the process and leave out the
/// fraction of a second; the process is still the word before the level.
#[test]
fn a_line_with_the_device_name_is_read_the_same() {
    let sending = "Nov 10 20:44:49 Test-iPhone atc(AirTrafficDevice)[53] <Notice>: Sending disk usage. lastUpdate=1156.07s ago";
    let full = FULL.replace("20:44:49.594555 ", "20:44:49 Test-iPhone ");
    assert_eq!(verdict(&[sending, &full]), Some(Refresh::Full));
    let plain =
        "Nov 10 20:44:49 Test-iPhone atc[53] <Notice>: Sending disk usage. lastUpdate=10.00s ago";
    let media = MEDIA.replace("atc(AirTrafficDevice)", "atc");
    assert_eq!(
        verdict(&[plain, &media]),
        Some(Refresh::Partial {
            since_full: Duration::from_secs(10)
        })
    );
}

/// 2026-11-16 23:42:33: a link opened 223 s after a full refresh got only the
/// disk-usage line, and the port did not stall.
#[test]
fn a_young_last_update_without_a_gathering_line_is_a_partial_refresh() {
    let after = |seconds: &str| {
        let mut verdict = Verdict::default();
        assert_eq!(verdict.feed(&sending(seconds)), None);
        verdict.without_gathering()
    };
    assert_eq!(
        after("223.32"),
        Some(Refresh::Partial {
            since_full: Duration::from_secs_f64(223.32)
        })
    );
    assert_eq!(
        after("539.99"),
        Some(Refresh::Partial {
            since_full: Duration::from_secs_f64(539.99)
        })
    );
    // Near or past the phone's 600 s threshold a full refresh may start.
    assert_eq!(after("540.00"), None);
    assert_eq!(after("816561530.19"), None);
    // Nothing from atc decides nothing.
    assert_eq!(Verdict::default().without_gathering(), None);
}
