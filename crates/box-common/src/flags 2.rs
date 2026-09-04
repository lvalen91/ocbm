//! Session-arbitration flags — the `/tmp` substrate the CarPlay and Android Auto sets share so a
//! live session is never interrupted by another connecting phone. See docs/androidauto/02_ARBITRATION.md.
//!
//! Today `session_supervisor.sh` arbitrates wired↔wireless CarPlay via `/tmp/carplay_transport`
//! (= "wireless" when the wireless arm owns the box). This module generalizes that to a single
//! projection OWNER covering all transports, so Android Auto folds into the same first-come-wins
//! model instead of a parallel path. `ProjectionOwner::as_str()` is byte-compatible with the shell:
//! "wireless" == WirelessCp, so an existing `carplay_transport` file still reads correctly.
//!
//! Portable (plain file writes); the shell reads the same paths.

use std::fs;
use std::path::Path;

/// Master app-presence gate (written by ocbmd on CT_SUBSCRIBE). Box idles until an app connects.
pub const HOST_PRESENT: &str = "/tmp/host_present";
/// Wired-USB phone presence (written by session_supervisor; mirrored to the host as SEV_PHONE_*).
pub const PHONE_PRESENT: &str = "/tmp/phone_present";
/// App-commanded radio kill switch (CT_RADIO).
pub const RADIO_OFF: &str = "/tmp/radio_off";
/// Single projection owner across ALL transports (supersedes the CarPlay-only `carplay_transport`).
pub const PROJECTION_OWNER: &str = "/tmp/projection_owner";
/// Legacy CarPlay-only ownership flag still written by the wireless arm (av.rs). Read for back-compat.
pub const CARPLAY_TRANSPORT: &str = "/tmp/carplay_transport";

/// Who owns the single phone-facing controller / projection session right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionOwner {
    /// Idle — no session; a new phone of either kind may claim the box.
    None,
    /// Wired CarPlay (iap2d + airplayd over NCM).
    WiredCp,
    /// Wireless CarPlay (carplay-wireless: BT + WiFi AP). Byte-compatible with the legacy "wireless".
    WirelessCp,
    /// Wired Android Auto (aa-bridge AOAP pump).
    WiredAa,
    /// Wireless Android Auto (aa-wireless: BT bootstrap + WiFi AP + TCP accept).
    WirelessAa,
}

impl ProjectionOwner {
    /// On-disk token. `WirelessCp` == "wireless" so an existing `carplay_transport` reads correctly.
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectionOwner::None => "",
            ProjectionOwner::WiredCp => "wired-cp",
            ProjectionOwner::WirelessCp => "wireless",
            ProjectionOwner::WiredAa => "wired-aa",
            ProjectionOwner::WirelessAa => "wireless-aa",
        }
    }

    pub fn from_str(s: &str) -> ProjectionOwner {
        match s.trim() {
            "wired-cp" => ProjectionOwner::WiredCp,
            "wireless" | "wireless-cp" => ProjectionOwner::WirelessCp,
            "wired-aa" => ProjectionOwner::WiredAa,
            "wireless-aa" => ProjectionOwner::WirelessAa,
            _ => ProjectionOwner::None,
        }
    }

    /// Is this a CarPlay transport (wired or wireless)? The AA arm refuses to arm while true.
    pub fn is_carplay(self) -> bool {
        matches!(self, ProjectionOwner::WiredCp | ProjectionOwner::WirelessCp)
    }

    /// Is this an Android Auto transport? The CarPlay arms refuse to arm while true.
    pub fn is_android_auto(self) -> bool {
        matches!(self, ProjectionOwner::WiredAa | ProjectionOwner::WirelessAa)
    }

    /// The `PM_*` byte ocbmd puts on the wire in `CT_PROJ_MODE`, so the host app learns WHICH
    /// transport owns the box (docs/androidauto/02_ARBITRATION.md). One mapping, shared by every box-side writer.
    pub fn wire_code(self) -> u8 {
        match self {
            ProjectionOwner::None => ocbm_proto::PM_NONE,
            ProjectionOwner::WiredCp => ocbm_proto::PM_WIRED_CP,
            ProjectionOwner::WirelessCp => ocbm_proto::PM_WIRELESS_CP,
            ProjectionOwner::WiredAa => ocbm_proto::PM_WIRED_AA,
            ProjectionOwner::WirelessAa => ocbm_proto::PM_WIRELESS_AA,
        }
    }
}

/// Read the current projection owner, preferring the unified flag and falling back to the legacy
/// CarPlay-only `carplay_transport` (so this works before the shell is migrated).
pub fn owner() -> ProjectionOwner {
    if let Ok(s) = fs::read_to_string(PROJECTION_OWNER) {
        let o = ProjectionOwner::from_str(&s);
        if o != ProjectionOwner::None {
            return o;
        }
    }
    match fs::read_to_string(CARPLAY_TRANSPORT) {
        Ok(s) => ProjectionOwner::from_str(&s),
        Err(_) => ProjectionOwner::None,
    }
}

/// Claim the session for `who` (first-come-wins is the caller's responsibility: check `owner()` first).
pub fn set_owner(who: ProjectionOwner) -> std::io::Result<()> {
    match who {
        ProjectionOwner::None => clear_owner(),
        _ => write_atomic(PROJECTION_OWNER, who.as_str()),
    }
}

/// Write `content` to `path` via a same-directory temp + `rename` (ocbmd's `write_flag_atomic`
/// idiom). `fs::write` truncates first, and every reader of these flags is a different process
/// polling them: one landing in that window reads "", which `ProjectionOwner::from_str` maps to
/// `None` — "the box is free to claim" — so a re-write of an owner could hand the session away
/// mid-claim. Rename is atomic: a reader sees the whole old value or the whole new one. The pid
/// suffix keeps the temp private to this writer, since ocbmd, aa-bridge and aa-wireless all write
/// `PROJECTION_OWNER` and a shared temp name would just move the torn window into the temp file.
fn write_atomic(path: &str, content: &str) -> std::io::Result<()> {
    let tmp = format!("{path}.tmp.{}", std::process::id());
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Release the session (idle). Removes the unified flag; leaves the legacy flag to its own writer.
pub fn clear_owner() -> std::io::Result<()> {
    match fs::remove_file(PROJECTION_OWNER) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Presence-flag helper: a flag file "exists and is non-'0'" == true (matches the shell convention).
pub fn is_set(path: &str) -> bool {
    match fs::read_to_string(Path::new(path)) {
        Ok(s) => {
            let t = s.trim();
            !t.is_empty() && t != "0"
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_roundtrip_and_legacy_compat() {
        assert_eq!(ProjectionOwner::from_str("wireless"), ProjectionOwner::WirelessCp);
        assert_eq!(ProjectionOwner::WirelessCp.as_str(), "wireless");
        assert_eq!(ProjectionOwner::from_str("wired-aa"), ProjectionOwner::WiredAa);
        assert!(ProjectionOwner::WiredCp.is_carplay());
        assert!(ProjectionOwner::WiredAa.is_android_auto());
        assert!(!ProjectionOwner::WiredAa.is_carplay());
    }

    /// The torn-read window itself is not observable from a single-threaded test; what IS checkable
    /// is that the value lands whole on both create and overwrite and that no `.tmp.<pid>` sibling
    /// survives — a leftover temp would mean the rename never happened and the reader is on stale
    /// data (the same silent failure as the empty window, one step removed).
    #[test]
    fn write_atomic_replaces_content_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("box-common-flags-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("projection_owner");
        let p = path.to_str().unwrap();

        write_atomic(p, ProjectionOwner::WirelessAa.as_str()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), ProjectionOwner::WirelessAa.as_str());
        // Overwrite with a SHORTER value: no tail of the longer previous owner may survive.
        write_atomic(p, ProjectionOwner::WirelessCp.as_str()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), ProjectionOwner::WirelessCp.as_str());
        assert_eq!(owner_from(&path), ProjectionOwner::WirelessCp);

        let strays: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(strays.is_empty(), "temp file left behind: {strays:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    fn owner_from(path: &Path) -> ProjectionOwner {
        ProjectionOwner::from_str(&fs::read_to_string(path).unwrap())
    }
}
