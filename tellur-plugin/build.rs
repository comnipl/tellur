use std::path::Path;
use std::process::Command;

mod fingerprint;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=fingerprint.rs");

    let out_dir = env("OUT_DIR");
    if let Some(lock_path) = fingerprint::find_cargo_lock(Path::new(&out_dir)) {
        println!("cargo:rerun-if-changed={}", lock_path.display());
    }

    let (rustc_release, rustc_commit) = rustc_version();
    let target = env("TARGET");
    let pkg_version = env("CARGO_PKG_VERSION");

    let lock_path = fingerprint::find_cargo_lock(Path::new(&out_dir));
    let fingerprint = fingerprint::build_fingerprint(
        &rustc_release,
        &rustc_commit,
        &target,
        &pkg_version,
        lock_path.as_deref(),
    );

    println!("cargo:rustc-env=TELLUR_ABI_FINGERPRINT={fingerprint}");
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|e| panic!("missing env var {name}: {e}"))
}

fn rustc_version() -> (String, String) {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let output = Command::new(&rustc)
        .arg("-vV")
        .output()
        .unwrap_or_else(|e| panic!("failed to run {rustc} -vV: {e}"));
    if !output.status.success() {
        panic!(
            "{rustc} -vV failed with status {}",
            output.status
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut release = None;
    let mut commit = None;
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix("release: ") {
            release = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("commit-hash: ") {
            commit = Some(value.to_owned());
        }
    }

    (
        release.unwrap_or_else(|| "unknown".to_owned()),
        commit.unwrap_or_else(|| "unknown".to_owned()),
    )
}
