#!/usr/bin/env python3
"""Fail if the Rust drift-guard fixture no longer matches the LIVE Swift emitter.

WHY THIS EXISTS. `tools/regen_app_yaml_fixture.py` made "RE-GENERATE the fixture if you
touch the emitter" an executable instruction, but nothing MADE anyone run it — so the
guard only bit if a human remembered. That is the same shape as the bug it guards: a
promise in a comment with no mechanism behind it. This script is the mechanism.

It regenerates the document from SettingsWindow.swift + VehicleConfig.swift and compares
it to the literal embedded in `the_apps_real_emitted_document_parses`
(crates/vendor/receiver/src/vehicle_config.rs). A mismatch means someone changed the
emitter without re-running the generator, so the Rust suite is now validating a document
the app no longer sends — green tests, unguarded wire.

Exit 0 = current. Exit 1 = stale (prints the diff). Exit 2 = could not check.

  python3 tools/check_app_yaml_fixture.py [repo-root]
"""
import subprocess, sys, os, difflib

root = sys.argv[1] if len(sys.argv) > 1 else '.'
gen_script = os.path.join(root, 'tools/regen_app_yaml_fixture.py')
rust_file = os.path.join(root, 'crates/vendor/receiver/src/vehicle_config.rs')
# Anchor on the shared const, NOT on a test body. It used to key off
# `let y = r#"` inside the drift-guard test; when that literal was promoted to a const
# the anchor silently matched a DIFFERENT literal later in the file and the check
# degraded to comparing the wrong thing. Anchor on the declaration itself.
TEST = 'const APP_EMITTED_DOCUMENT: &str = r#"'

for p in (gen_script, rust_file):
    if not os.path.exists(p):
        print(f"[fixture-check] SKIP — {p} not found", file=sys.stderr)
        sys.exit(2)

try:
    gen = subprocess.run([sys.executable, gen_script, root],
                         capture_output=True, text=True, timeout=300)
except Exception as e:                                    # noqa: BLE001
    print(f"[fixture-check] SKIP — generator could not run: {e}", file=sys.stderr)
    sys.exit(2)
if gen.returncode != 0:
    print("[fixture-check] SKIP — generator failed (xcrun swift unavailable?):", file=sys.stderr)
    print(gen.stderr[:800], file=sys.stderr)
    sys.exit(2)

src = open(rust_file).read()
if TEST not in src:
    print(f"[fixture-check] SKIP — {TEST!r} not found; was the const renamed or moved?",
          file=sys.stderr)
    sys.exit(2)
try:
    fixture = src.split(TEST, 1)[1].split('"#;', 1)[0]
except IndexError:
    print("[fixture-check] SKIP — could not locate the r#\"...\"# literal", file=sys.stderr)
    sys.exit(2)

if fixture == gen.stdout:
    print("[fixture-check] OK — the Rust fixture matches the live Swift emitter")
    sys.exit(0)

print("[fixture-check] *** STALE — the emitter changed and the fixture was not regenerated ***")
print("*** The Rust suite is validating a document the app no longer sends. Fix with:")
print("***   python3 tools/regen_app_yaml_fixture.py . "
      "   # then paste into the_apps_real_emitted_document_parses")
sys.stdout.writelines(difflib.unified_diff(
    fixture.splitlines(keepends=True), gen.stdout.splitlines(keepends=True),
    fromfile='rust fixture (committed)', tofile='swift emitter (live)'))
sys.exit(1)
