// Compile the libfdk-aac C shim into a static archive and link it + libfdk-aac.
// Dep-free: shells out to the C compiler/archiver resolved below, so we don't pull the `cc` crate.
//
// fdk-aac location: FDK_AAC_PREFIX points to a prefix with include/fdk-aac/*.h and lib/libfdk-aac.a
// (for the cross target, e.g. the armv7-musl build in scratchpad/fdk/install). When unset, we fall
// back to the system default (/usr/include/fdk-aac + a dynamic -lfdk-aac) — the historical behavior.
use std::process::Command;

/// cc-rs-style env lookup for a toolchain tool: `<BASE>_<target>` (hyphenated), then
/// `<BASE>_<target_with_underscores>` (what cargo-zigbuild 0.23 actually sets, e.g.
/// `CC_armv7_unknown_linux_musleabihf`), then `TARGET_<BASE>`, then plain `<BASE>`, then `default`.
/// Every candidate name is emitted as rerun-if-env-changed — including ones currently unset — so a
/// toolchain change (or a previously-unset higher-priority variable appearing) invalidates the
/// cached object instead of silently reusing a wrong-arch eld_shim.o. Reading only plain `CC` here
/// is what let a plain-cargo clippy run poison the armv7 cache with a host Mach-O object
/// (found + reproduced 2026-07-31).
fn tool(base: &str, target: &str, default: &str) -> String {
    let candidates = [
        format!("{base}_{target}"),
        format!("{base}_{}", target.replace('-', "_")),
        format!("TARGET_{base}"),
        base.to_string(),
    ];
    let mut found = None;
    for name in &candidates {
        println!("cargo:rerun-if-env-changed={name}");
        if found.is_none() {
            if let Ok(v) = std::env::var(name) {
                if !v.is_empty() {
                    found = Some(v);
                }
            }
        }
    }
    found.unwrap_or_else(|| default.into())
}

fn main() {
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let target = std::env::var("TARGET").expect("TARGET"); // cargo always sets this for build scripts
    let obj = format!("{out}/eld_shim.o");
    let lib = format!("{out}/libeldshim.a");
    // Full CC command may be "zig cc -target arm-linux-musleabihf"; split so args pass through.
    let cc = tool("CC", &target, "cc");
    let mut cc_parts = cc.split_whitespace();
    let cc_bin = cc_parts.next().unwrap_or("cc");
    let cc_args: Vec<String> = cc_parts.map(String::from).collect();
    let ar = tool("AR", &target, "ar");
    let mut ar_parts = ar.split_whitespace();
    let ar_bin = ar_parts.next().unwrap_or("ar");
    let ar_args: Vec<String> = ar_parts.map(String::from).collect(); // e.g. "zig ar" → ["ar"]
    println!("cargo:rerun-if-env-changed=FDK_AAC_PREFIX");
    let fdk_prefix = std::env::var("FDK_AAC_PREFIX").ok();

    let mut cmd = Command::new(cc_bin);
    cmd.args(&cc_args);
    cmd.args(["-c", "-O2", "-fPIC"]);
    if let Some(p) = &fdk_prefix {
        cmd.arg(format!("-I{p}/include")); // cross fdk-aac headers
    }
    cmd.args(["csrc/eld_shim.c", "-o", &obj]);
    let status = cmd.status().expect("failed to run C compiler (cc) for eld_shim.c");
    assert!(status.success(), "cc failed compiling csrc/eld_shim.c");

    let _ = std::fs::remove_file(&lib); // `ar crs` appends; start fresh
    let status = Command::new(ar_bin)
        .args(&ar_args)
        .args(["crs", &lib, &obj])
        .status()
        .expect("failed to run ar");
    assert!(status.success(), "ar failed creating libeldshim.a");

    println!("cargo:rustc-link-search=native={out}");
    println!("cargo:rustc-link-lib=static=eldshim");
    if let Some(p) = &fdk_prefix {
        // Statically link the cross-compiled libfdk-aac.a (musl-static box binary — no dynamic libs).
        println!("cargo:rustc-link-search=native={p}/lib");
        println!("cargo:rustc-link-lib=static=fdk-aac");
    } else {
        println!("cargo:rustc-link-lib=fdk-aac");
    }
    // NB: emitting rerun-if-env-changed disables cargo's default rerun heuristic, so these
    // rerun-if-changed lines are load-bearing — keep them.
    println!("cargo:rerun-if-changed=csrc/eld_shim.c");
    println!("cargo:rerun-if-changed=build.rs");
}
