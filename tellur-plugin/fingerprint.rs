//! Shared ABI fingerprint generation for the live-preview plugin boundary.
//!
//! Used by `build.rs` at compile time and by unit tests. Each side of the
//! dynamic-library boundary (host and plugin) embeds the fingerprint produced
//! from its own build graph so mismatched transitive deps (e.g. `bytes`) are
//! caught before Rust types cross the dlopen boundary.

#![allow(dead_code)] // Included by build.rs and lib; not every item is used in both.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Crates whose types may cross the host/plugin dylib boundary.
///
/// Extend this list when a new dependency's types are passed across the
/// `TimelineCollection` / `RasterImage` ABI (not for tellur-internal types
/// that stay on one side).
pub const BOUNDARY_CRATES: &[&str] = &["bytes"];

/// Walk upward from `start` (typically `OUT_DIR`) until a `Cargo.lock` is found.
pub fn find_cargo_lock(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join("Cargo.lock");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn unquote(value: &str) -> String {
    value.trim().trim_matches('"').to_owned()
}

/// Parse resolved versions for `names` from a `Cargo.lock` file body.
pub fn parse_crate_versions(lock: &str, names: &[&str]) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    let mut current_name: Option<String> = None;

    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            current_name = None;
            continue;
        }
        if let Some(rest) = line.strip_prefix("name = ") {
            current_name = Some(unquote(rest));
            continue;
        }
        if let Some(rest) = line.strip_prefix("version = ") {
            if let Some(name) = current_name.as_deref() {
                if names.contains(&name) {
                    result.insert(name.to_owned(), unquote(rest));
                }
            }
        }
    }

    for name in names {
        result
            .entry((*name).to_owned())
            .or_insert_with(|| "unknown".to_owned());
    }

    result
}

/// Build the fingerprint string embedded into host and plugin binaries.
pub fn build_fingerprint(
    rustc_release: &str,
    rustc_commit: &str,
    target: &str,
    pkg_version: &str,
    lock_path: Option<&Path>,
) -> String {
    let mut parts = vec![
        format!("rustc={rustc_release}/{rustc_commit}"),
        format!("target={target}"),
        format!("tellur-plugin={pkg_version}"),
    ];

    if let Some(lock_path) = lock_path {
        parts.push("lock=found".to_owned());
        let lock_content = std::fs::read_to_string(lock_path).unwrap_or_default();
        let versions = parse_crate_versions(&lock_content, BOUNDARY_CRATES);
        for crate_name in BOUNDARY_CRATES {
            let version = versions
                .get(*crate_name)
                .map(String::as_str)
                .unwrap_or("unknown");
            parts.push(format!("{crate_name}={version}"));
        }
    } else {
        parts.push("lock=unknown".to_owned());
        for crate_name in BOUNDARY_CRATES {
            parts.push(format!("{crate_name}=unknown"));
        }
    }

    parts.join(" ")
}

/// Parse `key=value` tokens from a fingerprint string.
pub fn parse_fingerprint_kv(fingerprint: &str) -> BTreeMap<String, String> {
    fingerprint
        .split_whitespace()
        .filter_map(|token| {
            let (key, value) = token.split_once('=')?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}

/// Suggest remediation when host and plugin fingerprints differ.
pub fn remediation_hint(host: &str, plugin: &str) -> String {
    let host_map = parse_fingerprint_kv(host);
    let plugin_map = parse_fingerprint_kv(plugin);
    let mut hints = Vec::new();

    if host_map.get("lock") == Some(&"unknown".to_owned())
        || plugin_map.get("lock") == Some(&"unknown".to_owned())
    {
        hints.push(
            "ensure the project has a Cargo.lock and rebuild the plugin from that directory"
                .to_owned(),
        );
    }

    for crate_name in BOUNDARY_CRATES {
        if host_map.get(*crate_name) != plugin_map.get(*crate_name) {
            if let Some(host_ver) = host_map.get(*crate_name) {
                if host_ver != "unknown" {
                    hints.push(format!("cargo update -p {crate_name} --precise {host_ver}"));
                }
            }
        }
    }

    if host_map.get("tellur-plugin") != plugin_map.get("tellur-plugin") {
        hints.push(
            "update the tellur dependency to the same version as the tellur live host".to_owned(),
        );
    }

    if host_map.get("rustc") != plugin_map.get("rustc") {
        hints.push("rebuild the plugin with the same Rust toolchain as the tellur host".to_owned());
    }

    if hints.is_empty() {
        "rebuild the plugin with the same tellur version and Cargo.lock as the live host".to_owned()
    } else {
        hints.join("; ")
    }
}
