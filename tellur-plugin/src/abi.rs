//! Runtime ABI fingerprint validation for live-preview plugin loading.

use std::fmt;

#[path = "../fingerprint.rs"]
mod fingerprint;

/// Symbol exported by timeline plugins for ABI fingerprint checks.
pub const ABI_FINGERPRINT_SYMBOL: &[u8] = b"tellur_abi_fingerprint_v1\0";

/// Fingerprint of the currently linked `tellur-plugin` build graph.
pub const ABI_FINGERPRINT: &str = env!("TELLUR_ABI_FINGERPRINT");

/// NUL-terminated fingerprint bytes for the C ABI export symbol.
#[doc(hidden)]
pub static ABI_FINGERPRINT_C: &[u8] = concat!(env!("TELLUR_ABI_FINGERPRINT"), "\0").as_bytes();

/// Signature of [`ABI_FINGERPRINT_SYMBOL`].
pub type AbiFingerprintFn = unsafe extern "C" fn() -> *const std::os::raw::c_char;

/// Error when a plugin's ABI fingerprint does not match the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiMismatchError {
    pub host: String,
    pub plugin: String,
    pub hint: String,
}

impl fmt::Display for AbiMismatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "plugin ABI fingerprint mismatch\n  host:   {}\n  plugin: {}\n  fix: {}",
            self.host, self.plugin, self.hint
        )
    }
}

impl std::error::Error for AbiMismatchError {}

/// Validate a plugin fingerprint string against this host build.
pub fn validate_plugin_fingerprint(plugin: &str) -> Result<(), AbiMismatchError> {
    if plugin == ABI_FINGERPRINT {
        return Ok(());
    }

    Err(AbiMismatchError {
        host: ABI_FINGERPRINT.to_owned(),
        plugin: plugin.to_owned(),
        hint: fingerprint::remediation_hint(ABI_FINGERPRINT, plugin),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fingerprint::{find_cargo_lock, parse_crate_versions, remediation_hint};
    use std::path::Path;

    #[test]
    fn validate_accepts_host_fingerprint() {
        validate_plugin_fingerprint(ABI_FINGERPRINT).expect("host fingerprint matches");
    }

    #[test]
    fn validate_rejects_mismatched_fingerprint() {
        let err =
            validate_plugin_fingerprint("rustc=0.0.0/000 target=unknown tellur-plugin=0.0.0 lock=unknown bytes=unknown")
                .expect_err("mismatch");
        assert_ne!(err.host, err.plugin);
        assert!(!err.hint.is_empty());
    }

    #[test]
    fn find_cargo_lock_walks_up_from_out_dir() {
        let lock = find_cargo_lock(Path::new(env!("CARGO_MANIFEST_DIR")));
        assert!(lock.is_some(), "workspace Cargo.lock should be discoverable");
    }

    #[test]
    fn remediation_suggests_precise_update_for_bytes() {
        let host = "rustc=1.95.0/abc target=x86_64-unknown-linux-gnu tellur-plugin=0.1.0 lock=found bytes=1.11.1";
        let plugin = "rustc=1.95.0/abc target=x86_64-unknown-linux-gnu tellur-plugin=0.1.0 lock=found bytes=1.12.0";
        let hint = remediation_hint(host, plugin);
        assert!(hint.contains("cargo update -p bytes --precise 1.11.1"));
    }

    #[test]
    fn parse_crate_versions_reads_lock_entries() {
        let lock = r#"
[[package]]
name = "bytes"
version = "1.11.1"
"#;
        let versions = parse_crate_versions(lock, fingerprint::BOUNDARY_CRATES);
        assert_eq!(versions.get("bytes"), Some(&"1.11.1".to_owned()));
    }
}
