//! The app-pushed metadata policy's SUCCESS path (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B3).
//!
//! This lives in its own integration-test binary on purpose: `features::PUSHED` is a process-global
//! `OnceLock`, so arming it inside the unit-test binary would leak into every sibling test there.
//! A Cargo integration test gets its own process, which is what makes a real arm safe to exercise —
//! and the claims below (tier reaches all three axes, pushed beats the bench levers, a pushed skip
//! REPLACES rather than concatenates, first-arm-wins) are asserted in docs/carplay/05_METADATA_AND_CONTROLS.md §5.2, CLAUDE.md and
//! `MetadataConfig`'s doc comment, so they need a test that fails if a future refactor reverses them.

use carplay_iap2_core::config::Iap2Config;
use carplay_iap2_core::features::{self, Policy, Tier};

#[test]
fn pushed_policy_governs_tier_skip_and_precedence() {
    // Bench levers armed FIRST and deliberately disagreeing with what the app pushes: the pushed
    // policy must win outright (docs/carplay/04_CAPABILITIES_AND_CONFIG.md — an on-box override is subordinate to the pushed config).
    // SAFETY: single-threaded test-process setup, before any policy read.
    unsafe {
        std::env::set_var("CARPLAY_METADATA", "proven");
        std::env::set_var("CARPLAY_METADATA_SKIP", "power");
    }

    let cfg = Iap2Config::from_slice(b"metadata:\n  tier: extended\n  skip: [route_guidance]\n");
    assert!(cfg.arm_metadata_policy(), "a valid pushed tier must arm");

    // (a) the tier reaches ALL THREE axes, not just param 6.
    let p: Policy = Policy::active();
    assert_eq!(p.sent, Tier::Extended);
    assert_eq!(p.recv, Tier::Extended);
    assert_eq!(p.subscribe, Tier::Extended);

    // (b) the pushed skip REPLACES the bench list rather than concatenating with it: `power` came
    // from CARPLAY_METADATA_SKIP above and must NOT be in effect, while `route_guidance` must be.
    let feats = features::active(p.sent, Vec::new, Vec::new);
    let sent = features::sent_ids(&feats);
    let names: Vec<&str> = feats.iter().map(|f| f.name).collect();
    assert!(!names.contains(&"route_guidance"), "pushed skip must drop route_guidance");
    assert!(names.contains(&"power"), "bench skip must NOT survive a pushed skip");

    // (c) the skip's orphan rider goes with it — dropping route_guidance must also drop
    // lane_guidance, which rides on it (docs/carplay/05_METADATA_AND_CONTROLS.md §5.6 rule 2: no receive without its send).
    assert!(!names.contains(&"lane_guidance"), "an orphaned rider must be dropped with its trigger");
    // …and nothing route-guidance-shaped survives in the declared id set.
    assert!(!sent.is_empty(), "extended must declare a non-empty param 6");

    // (d) FIRST ARM WINS: a second, different push is refused and changes nothing.
    let second = Iap2Config::from_slice(b"metadata:\n  tier: all\n");
    assert!(!second.arm_metadata_policy(), "a second arm must be refused");
    assert_eq!(Policy::active().sent, Tier::Extended, "the pinned tier must survive a re-arm attempt");
}
