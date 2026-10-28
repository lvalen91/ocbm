//! Every Android Auto lever gate in aa-bridge, as a NAMED SITE of a declared kind.
//!
//! The lever has two readings (`box_common::cfg::AaGate`): LAUNCH ("may AA start here?", explicit
//! `android_auto: true` only) and STAND-DOWN ("may an already-started path keep going?", yes unless
//! an existing document stops requesting it — an absent document is an ocbmd restart or `go_idle`,
//! not a withdrawal). Using the wrong one is the bug: a launch site reading stand-down would start
//! AA with no config at all (D1 regression); a stand-down site reading launch would end a live
//! session on every ocbmd restart. The tests below pin the kind of each site.
//!
//! | Site (const)              | Where                                                    | Kind       |
//! |---------------------------|----------------------------------------------------------|------------|
//! | `WIRED_RESIDENT_WAKE`     | main.rs `wait_for_wired_work` (resident bridge re-arms)  | Launch     |
//! | `WIRED_LOOP_TOP`          | main.rs `run_wired_arm`, top of each pass                | StandDown  |
//! | `PREPARE_BACKOFF_WAKE`    | main.rs `nap_while_enabled` (wake early from backoff)    | StandDown  |
//! | `ANNOUNCE_WAIT`           | main.rs `accept_while_wanted` (claimed/unclaimed wait)   | StandDown  |
//! | `WIRED_MID_SESSION`       | main.rs `serve_session` watchdog                         | StandDown  |
//! | `WIRELESS_ADMIT`          | wireless.rs `serve_one`, per phone that dials the AP     | Launch     |
//! | `WIRELESS_HOST_WAIT`      | wireless.rs waiting for the host to take the phone       | StandDown  |
//! | `WIRELESS_MID_SESSION`    | wireless.rs session watchdog                             | StandDown  |
//!
//! NOT routed through this module, by owner decision (2026-10-19): the two CLAIM re-checks in
//! main.rs `run_wired_arm` — after `prepare_accessory` ("android_auto turned off while we
//! prepared") and the unclaimed-client claim (`Ok(acc2) if cfg::aa_withdrawn()`). Both call
//! `cfg::aa_withdrawn()` directly, i.e. StandDown, and stay textually untouched because they
//! protect a session that is forming across an ocbmd restart. They are therefore NOT covered by
//! these tests; a change to them is a change to an owner decision, not a refactor.

use box_common::cfg::{self, AaGate};

pub(crate) const WIRED_RESIDENT_WAKE: AaGate = AaGate::Launch;
pub(crate) const WIRED_LOOP_TOP: AaGate = AaGate::StandDown;
pub(crate) const PREPARE_BACKOFF_WAKE: AaGate = AaGate::StandDown;
pub(crate) const ANNOUNCE_WAIT: AaGate = AaGate::StandDown;
pub(crate) const WIRED_MID_SESSION: AaGate = AaGate::StandDown;
pub(crate) const WIRELESS_ADMIT: AaGate = AaGate::Launch;
pub(crate) const WIRELESS_HOST_WAIT: AaGate = AaGate::StandDown;
pub(crate) const WIRELESS_MID_SESSION: AaGate = AaGate::StandDown;

/// May Android Auto proceed at `site`, given the config on disk right now?
pub(crate) fn aa_allows(site: AaGate) -> bool {
    site.allows(cfg::aa_lever())
}

#[cfg(test)]
mod tests {
    use super::*;
    use box_common::cfg::aa_lever_of;

    const ON: Option<&str> = Some("wireless: true\nandroid_auto: true\nwifi_ap: true\n");
    const OFF: Option<&str> = Some("wireless: true\nandroid_auto: false\nwifi_ap: true\n");
    const NO_KEY: Option<&str> = Some("wireless: true\nwifi_ap: true\n");
    const NO_DOC: Option<&str> = None;

    fn allows(site: AaGate, doc: Option<&str>) -> bool {
        site.allows(aa_lever_of(doc))
    }

    /// Launch sites start AA only on an explicit request; an absent document must NOT start it.
    #[test]
    fn launch_sites_need_an_explicit_request() {
        for (name, site) in [("WIRED_RESIDENT_WAKE", WIRED_RESIDENT_WAKE), ("WIRELESS_ADMIT", WIRELESS_ADMIT)] {
            assert!(allows(site, ON), "{name}");
            assert!(!allows(site, OFF), "{name}: explicit false");
            assert!(!allows(site, NO_KEY), "{name}: key missing");
            assert!(!allows(site, NO_DOC), "{name}: no document is not a request (D1)");
        }
    }

    /// Stand-down sites stop only on an explicit withdrawal; an absent document (ocbmd restart /
    /// go_idle mid-drive) must NOT end a running path.
    #[test]
    fn stand_down_sites_survive_a_missing_document() {
        for (name, site) in [
            ("WIRED_LOOP_TOP", WIRED_LOOP_TOP),
            ("PREPARE_BACKOFF_WAKE", PREPARE_BACKOFF_WAKE),
            ("ANNOUNCE_WAIT", ANNOUNCE_WAIT),
            ("WIRED_MID_SESSION", WIRED_MID_SESSION),
            ("WIRELESS_HOST_WAIT", WIRELESS_HOST_WAIT),
            ("WIRELESS_MID_SESSION", WIRELESS_MID_SESSION),
        ] {
            assert!(allows(site, ON), "{name}");
            assert!(allows(site, NO_DOC), "{name}: an absent document is not a withdrawal");
            assert!(!allows(site, OFF), "{name}: explicit false stops it");
            assert!(!allows(site, NO_KEY), "{name}: a document without the key stops it");
        }
    }
}
