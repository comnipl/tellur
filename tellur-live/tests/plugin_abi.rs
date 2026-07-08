//! Integration tests for live-preview plugin ABI loading.

use std::path::PathBuf;

use tellur_live::HotReloadPlugin;

fn demo_plugin_path() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_owned());
    let path = manifest_dir.join(format!(
        "../target/{profile}/examples/libdemo_timeline_plugin.so"
    ));
    if path.is_file() {
        Some(path)
    } else {
        None
    }
}

#[test]
fn loads_demo_plugin_when_built() {
    let Some(plugin_path) = demo_plugin_path() else {
        eprintln!(
            "skipping loads_demo_plugin_when_built: build the example first with \
             `cargo build -p tellur-live --example demo_timeline_plugin`"
        );
        return;
    };

    let mut loader = HotReloadPlugin::new(&plugin_path);
    loader
        .reload_if_changed()
        .expect("demo plugin should load when host and plugin share the workspace lock");
    let collection = loader.collection().expect("collection");
    assert!(!collection.timelines().is_empty());
}

#[test]
fn rejects_mismatched_abi_fingerprint() {
    let err = tellur_plugin::validate_plugin_fingerprint(
        "rustc=0.0.0/000 target=unknown tellur-plugin=0.0.0 lock=unknown bytes=unknown",
    )
    .expect_err("host fingerprint should reject a foreign plugin fingerprint");
    assert!(err.to_string().contains("plugin ABI fingerprint mismatch"));
}
