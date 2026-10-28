//! The app-pushed config file, and the levers the box reads out of it.
//!
//! `ocbmd` rewrites `/tmp/carplay_cfg.yaml` on every `CT_SUBSCRIBE` (`write_cfg_file`) — it is the
//! app's ephemeral, app-driven configuration (docs/carplay/04_CAPABILITIES_AND_CONFIG.md: anything configurable about projection is
//! app-driven; the box presents what the app pushes). Two consumers read it with no YAML parser at
//! all: `session_supervisor.sh` (raw `grep`), `aa-bridge` (since the F3 fix) and `btd` (the Android
//! Auto SDP record and RFCOMM channel-4 accept, since 2026-10-19).
//!
//! This module is the ONE definition of the lever spellings and their default sense on the Rust
//! side, mirroring what `flags::ProjectionOwner::wire_code()` does for the owner tokens. The shell
//! cannot link it, so `session_supervisor.sh`'s `aa_enabled()` / `wireless_enabled()` greps are the
//! deliberate second copy — keep the two in step, and note the matching is specified to be
//! byte-compatible with those greps (see `line_sets`). `tools/run_tests.sh` runs the shell's
//! `aa_enabled()` against `crates/box-common/tests/fixtures/aa_lever_cases.txt`, the same fixture
//! `aa_lever_matches_the_shared_shell_fixture` below reads, so a drift fails the suite.

use std::fs;

/// The app-pushed ephemeral config. `ocbmd::write_cfg_file` owns it; everything else only reads.
pub const CARPLAY_CFG_FILE: &str = "/tmp/carplay_cfg.yaml";

/// What the pushed config says about Android Auto.
///
/// Android Auto is EXPLICIT OPT-IN on the box (owner decision D1, 2026-10-19,
/// docs/androidauto/02_ARBITRATION.md): only a document carrying `android_auto: true` (and no
/// `android_auto: false` line) requests it. Until then a missing file or a missing key meant ON —
/// the box deciding a configurable value the app owns (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AaLever {
    /// An existing document says `android_auto: true` and nowhere says `false`.
    Requested,
    /// An existing document does NOT request it: an explicit `false`, the key missing (an older or
    /// AA-unaware host), or a value that is neither.
    NotRequested,
    /// No document at all (absent / unreadable). NORMAL between apps, and also a TRANSIENT
    /// mid-session state: a restarted ocbmd unlinks the file at startup and `go_idle` deletes it,
    /// both before the same host re-SUBSCRIBEs. It is neither a request nor a withdrawal.
    NoConfig,
}

/// The two readings of [`AaLever`] a call site can apply. Every Android Auto gate on the box is one
/// of these, and choosing the wrong one is the bug class this type exists to make testable (each
/// consumer names its sites as constants of this type and unit-tests them; see btd's `aa_gate_tests`
/// and aa-bridge's `gates`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AaGate {
    /// May Android Auto START here? Only on an explicit request ([`aa_requested`]).
    Launch,
    /// May an ALREADY-STARTED path keep going? Yes unless an existing document stops requesting it
    /// ([`aa_withdrawn`]); an absent document is not a withdrawal.
    StandDown,
}

impl AaGate {
    /// `true` = Android Auto may proceed at a site of this kind, given `lever`.
    pub fn allows(self, lever: AaLever) -> bool {
        match self {
            AaGate::Launch => lever == AaLever::Requested,
            AaGate::StandDown => lever != AaLever::NotRequested,
        }
    }
}

/// Classify a document's text (`None` = no document). Pure, so the shared-fixture parity test and
/// the unit tests can drive it without `/tmp`.
pub fn aa_lever_of(text: Option<&str>) -> AaLever {
    let Some(text) = text else {
        return AaLever::NoConfig;
    };
    let says = |v: &[u8]| text.lines().any(|l| line_sets(l, "android_auto", v));
    if says(b"true") && !says(b"false") {
        AaLever::Requested
    } else {
        AaLever::NotRequested
    }
}

/// [`aa_lever_of`] over [`CARPLAY_CFG_FILE`]. Read as BYTES and decoded lossily: the file is
/// host-pushed, and `read_to_string` would turn one invalid UTF-8 byte anywhere in it into "no
/// document", which the grep (byte-oriented) would not.
pub fn aa_lever() -> AaLever {
    let text = fs::read(CARPLAY_CFG_FILE).ok().map(|b| String::from_utf8_lossy(&b).into_owned());
    aa_lever_of(text.as_deref())
}

/// LAUNCH / CLAIM gate: may Android Auto START? Only on an explicit request.
///
/// Use at every point that would begin something — the resident bridge's `wait_for_wired_work`,
/// the wireless listener's per-phone accept, btd's SDP record and channel-4 accept. Equivalent to
/// the shell's `aa_enabled()` (explicit `true`, no explicit `false`).
pub fn aa_requested() -> bool {
    AaGate::Launch.allows(aa_lever())
}

/// STAND-DOWN gate: has a live / already-started Android Auto path been TOLD to stop?
///
/// Deliberately not `!aa_requested()`: an ABSENT file is not a withdrawal. A restarted ocbmd and
/// `go_idle` both delete the file mid-drive, and if that counted, every ocbmd restart would end a
/// live AA session at once — faster than today, where host departure is decided (debounced) by
/// `host_gone_confirmed` / `accept_while_wanted`'s `HOST_GONE_POLLS`. A document that exists and
/// does not request AA (explicit `false`, or a new host whose document has no key) IS a withdrawal.
pub fn aa_withdrawn() -> bool {
    !AaGate::StandDown.allows(aa_lever())
}

/// What the shell's `[[:space:]]` matches inside ONE line (POSIX `isspace` in the C locale: space,
/// `\t`, `\n`, `\v`, `\f`, `\r`; `\n` cannot occur within a line on either side, it is listed for
/// completeness). NOT `char::is_ascii_whitespace`, which omits `\v` (U+000B) — that difference was
/// a real shell/Rust drift (`\vandroid_auto: true` read as requested by the grep only) until
/// 2026-10-19. Non-ASCII spaces are outside both matchers' contract: the shell runs in the C locale
/// on the box, and the app never emits them.
const POSIX_SPACE: [char; 6] = [' ', '\t', '\n', '\x0B', '\x0C', '\r'];

/// Does `line` set `key` to a value starting with `value` (ASCII case-insensitive)?
///
/// Deliberately as dumb as the shell's `grep -qiE '^[[:space:]]*<key>:[[:space:]]*<value>'`, and
/// for the same reason: two consumers must agree on what the app pushed, and parity with the copy
/// that cannot be shared is worth more than YAML correctness. So: leading blanks allowed, key and
/// value case-insensitive, trailing junk after the value ignored, and a `#`-commented line is NOT
/// treated as a comment (neither is it by the grep, because a comment cannot start at the key
/// either way).
fn line_sets(line: &str, key: &str, value: &[u8]) -> bool {
    let line = line.trim_start_matches(POSIX_SPACE);
    match line.get(..key.len()) {
        Some(head) if head.eq_ignore_ascii_case(key) => {}
        _ => return false,
    }
    let Some(rest) = line[key.len()..].strip_prefix(':') else {
        return false;
    };
    let rest = rest.trim_start_matches(POSIX_SPACE);
    // Compare BYTES, not a `str` range: `rest[..5]` panics when byte 5 lands inside a multibyte
    // char (`android_auto: faaaé`), and the file is host-pushed into a `panic = "abort"` daemon.
    rest.as_bytes().get(..value.len()).is_some_and(|h| h.eq_ignore_ascii_case(value))
}

#[cfg(test)]
mod tests {
    use super::{aa_lever_of, line_sets, AaGate, AaLever};

    #[test]
    fn gate_truth_table() {
        use AaLever::*;
        assert!(AaGate::Launch.allows(Requested));
        assert!(!AaGate::Launch.allows(NotRequested));
        assert!(!AaGate::Launch.allows(NoConfig), "D1: no document is not a request");
        assert!(AaGate::StandDown.allows(Requested));
        assert!(!AaGate::StandDown.allows(NotRequested));
        assert!(AaGate::StandDown.allows(NoConfig), "an ocbmd restart / go_idle must not end a live session");
    }

    fn sets_false(line: &str) -> bool {
        line_sets(line, "android_auto", b"false")
    }

    #[test]
    fn explicit_false_in_any_shell_accepted_spelling() {
        for line in [
            "android_auto: false",
            "  android_auto:false",
            "\tandroid_auto:   FALSE",
            "android_auto: False   # off for this trip",
            "Android_Auto: false",
        ] {
            assert!(sets_false(line), "should read as false: {line:?}");
        }
    }

    #[test]
    fn everything_else_is_not_an_explicit_false() {
        for line in [
            "android_auto: true",
            "android_auto:",
            "android_auto_debug: false",   // a longer key is not this key
            "wireless: false",             // a different lever
            "# android_auto: false",       // the key does not start the line
            "  nested:",
            "",
            "android_auto: faaaé",         // multibyte value: byte 5 is mid-char, must not panic
            "android_auté: false",         // multibyte key
        ] {
            assert!(!sets_false(line), "should NOT read as false: {line:?}");
        }
    }

    /// The same no-panic property for the `true` comparison (4 bytes, so a 3-byte prefix + a
    /// multibyte char is the boundary case).
    #[test]
    fn multibyte_values_never_panic_either_comparison() {
        for line in ["android_auto: tré", "android_auto: é", "android_auto: trué", "android_auto:é"] {
            assert!(!line_sets(line, "android_auto", b"true"), "{line:?}");
            assert!(!line_sets(line, "android_auto", b"false"), "{line:?}");
        }
    }

    #[test]
    fn opt_in_semantics() {
        // D1: absent key / absent file are NOT a request.
        assert_eq!(aa_lever_of(None), AaLever::NoConfig);
        assert_eq!(aa_lever_of(Some("")), AaLever::NotRequested);
        assert_eq!(aa_lever_of(Some("wireless: true\nwifi_ap: true\n")), AaLever::NotRequested);
        assert_eq!(aa_lever_of(Some("name: X\nandroid_auto: true\n")), AaLever::Requested);
        assert_eq!(aa_lever_of(Some("android_auto: false\n")), AaLever::NotRequested);
        // Garbage is not a request.
        assert_eq!(aa_lever_of(Some("android_auto: yes\n")), AaLever::NotRequested);
        // Contradictory document: an explicit false wins (fail toward OFF, matching the shell).
        assert_eq!(aa_lever_of(Some("android_auto: true\nandroid_auto: false\n")), AaLever::NotRequested);
    }

    /// The two POSIX `[[:space:]]` members `char::is_ascii_whitespace` misses or a 3-char list
    /// missed: `\v` and `\f`, before the key and after the colon. The shell's grep accepts both.
    #[test]
    fn vertical_tab_and_form_feed_are_blanks_like_the_grep() {
        for doc in [
            "\x0Bandroid_auto: true",
            "\x0Candroid_auto: true",
            "android_auto:\x0Btrue",
            "android_auto:\x0Ctrue",
            " \t\x0B\x0C\randroid_auto: true",
        ] {
            assert_eq!(aa_lever_of(Some(doc)), AaLever::Requested, "{doc:?}");
        }
        // …and a \v/\f-indented `false` is an explicit false (so it still wins).
        assert_eq!(aa_lever_of(Some("android_auto: true\n\x0Bandroid_auto: false")), AaLever::NotRequested);
        assert_eq!(aa_lever_of(Some("android_auto: true\n\x0Candroid_auto: false")), AaLever::NotRequested);
    }

    /// Shell/Rust parity. `crates/box-common/tests/fixtures/aa_lever_cases.txt` is ALSO fed, case by case, to the
    /// `aa_enabled()` extracted from `tools/session_supervisor.sh` by `tools/run_tests.sh`; both
    /// sides assert the same expected column, so if either matcher drifts one of the two fails.
    #[test]
    fn aa_lever_matches_the_shared_shell_fixture() {
        let fx = include_str!("../tests/fixtures/aa_lever_cases.txt");
        let mut n = 0;
        for raw in fx.lines() {
            if raw.starts_with('#') || raw.trim().is_empty() {
                continue;
            }
            let (want, doc) = raw.split_once('|').expect("fixture line is `<0|1>|<document>`");
            // `\n` in the fixture separates document lines; `\t` `\v` `\f` `\r` are the other
            // POSIX spaces (same decoding as the shell side's `printf '%b'`).
            let doc = doc
                .replace("\\n", "\n")
                .replace("\\t", "\t")
                .replace("\\v", "\x0B")
                .replace("\\f", "\x0C")
                .replace("\\r", "\r");
            let got = aa_lever_of(Some(&doc)) == AaLever::Requested;
            assert_eq!(got, want == "1", "fixture case {raw:?}");
            n += 1;
        }
        assert!(n >= 10, "fixture lost its cases ({n})");
    }
}
