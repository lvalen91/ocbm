//! The app-pushed config file, and the levers the box reads out of it.
//!
//! `ocbmd` rewrites `/tmp/carplay_cfg.yaml` on every `CT_SUBSCRIBE` (`write_cfg_file`) — it is the
//! app's ephemeral, app-driven configuration (docs/carplay/04_CAPABILITIES_AND_CONFIG.md: anything configurable about projection is
//! app-driven; the box presents what the app pushes). Two consumers read it with no YAML parser at
//! all: `session_supervisor.sh` (raw `grep`) and, since the F3 fix, `aa-bridge`.
//!
//! This module is the ONE definition of the lever spellings and their default sense on the Rust
//! side, mirroring what `flags::ProjectionOwner::wire_code()` does for the owner tokens. The shell
//! cannot link it, so `session_supervisor.sh`'s `aa_enabled()` / `wireless_enabled()` greps are the
//! deliberate second copy — keep the two in step, and note the matching is specified to be
//! byte-compatible with those greps (see `key_is_false`).

use std::fs;

/// The app-pushed ephemeral config. `ocbmd::write_cfg_file` owns it; everything else only reads.
pub const CARPLAY_CFG_FILE: &str = "/tmp/carplay_cfg.yaml";

/// Is Android Auto enabled? Default **ON**: a missing file, a missing key or `android_auto: true`
/// all mean enabled, so a box that has never been told otherwise still projects a plugged-in
/// Android phone. Only an explicit `android_auto: false` opts out (the app's Settings ▸ Android
/// Auto toggle, docs/host/02_ANDROID_AUTO.mde).
///
/// Equivalent to the shell's
/// `aa_enabled() { ! grep -qiE '^[[:space:]]*android_auto:[[:space:]]*false' "$WIRELESS_CFG"; }`.
pub fn aa_enabled() -> bool {
    !key_is_false(CARPLAY_CFG_FILE, "android_auto")
}

/// Does any line of `path` set `key` to `false`?
///
/// Deliberately as dumb as the shell's `grep -qiE '^[[:space:]]*<key>:[[:space:]]*false'`, and for
/// the same reason: two consumers must agree on what the app pushed, and parity with the copy that
/// cannot be shared is worth more than YAML correctness. So: leading blanks allowed, key and value
/// case-insensitive, trailing junk after `false` ignored, and a `#`-commented line is NOT treated as
/// a comment (neither is it by the grep, because a comment cannot start at the key either way).
/// Unreadable file => not false => the caller's default applies.
fn key_is_false(path: &str, key: &str) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    text.lines().any(|line| line_sets_false(line, key))
}

fn line_sets_false(line: &str, key: &str) -> bool {
    const BLANK: [char; 3] = [' ', '\t', '\r'];
    let line = line.trim_start_matches(BLANK);
    match line.get(..key.len()) {
        Some(head) if head.eq_ignore_ascii_case(key) => {}
        _ => return false,
    }
    let Some(rest) = line[key.len()..].strip_prefix(':') else {
        return false;
    };
    let rest = rest.trim_start_matches(BLANK);
    rest.len() >= 5 && rest[..5].eq_ignore_ascii_case("false")
}

#[cfg(test)]
mod tests {
    use super::line_sets_false;

    #[test]
    fn explicit_false_in_any_shell_accepted_spelling() {
        for line in [
            "android_auto: false",
            "  android_auto:false",
            "\tandroid_auto:   FALSE",
            "android_auto: False   # off for this trip",
            "Android_Auto: false",
        ] {
            assert!(line_sets_false(line, "android_auto"), "should read as false: {line:?}");
        }
    }

    #[test]
    fn everything_else_leaves_the_default_alone() {
        for line in [
            "android_auto: true",
            "android_auto:",
            "android_auto_debug: false",   // a longer key is not this key
            "wireless: false",             // a different lever
            "# android_auto: false",       // the key does not start the line
            "  nested:",
            "",
        ] {
            assert!(!line_sets_false(line, "android_auto"), "should NOT read as false: {line:?}");
        }
    }
}
