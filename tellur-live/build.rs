// Ensure the Vite-built web bundle exists before compiling the Rust
// server, which embeds index.html / assets/index.js / assets/index.css
// via include_bytes!. When the bundle is missing we run `npm install`
// (if needed) and `npm run build` in tellur-live/web. The opt-out
// variable TELLUR_SKIP_WEB_BUILD=1 skips the npm step entirely; in that
// case we synthesise placeholder files so include_bytes! still compiles.
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env_var("CARGO_MANIFEST_DIR"));
    let web_dir = manifest_dir.join("web");
    let dist_dir = web_dir.join("dist");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=TELLUR_SKIP_WEB_BUILD");
    println!("cargo:rerun-if-changed={}", web_dir.join("package.json").display());
    println!("cargo:rerun-if-changed={}", web_dir.join("vite.config.ts").display());
    println!("cargo:rerun-if-changed={}", web_dir.join("index.html").display());
    track_dir(&web_dir.join("src"));

    if has_required_assets(&dist_dir) {
        return;
    }

    if std::env::var_os("TELLUR_SKIP_WEB_BUILD").is_some() {
        ensure_placeholder_bundle(&dist_dir);
        return;
    }

    build_web(&web_dir);

    if !has_required_assets(&dist_dir) {
        panic!(
            "web build did not produce expected files under {}. \
             Set TELLUR_SKIP_WEB_BUILD=1 to bypass and embed placeholders.",
            dist_dir.display()
        );
    }
}

fn env_var(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|e| panic!("missing required env var {name}: {e}"))
}

fn has_required_assets(dist: &Path) -> bool {
    dist.join("index.html").is_file()
        && dist.join("assets/index.js").is_file()
        && dist.join("assets/index.css").is_file()
}

fn ensure_placeholder_bundle(dist: &Path) {
    let assets = dist.join("assets");
    std::fs::create_dir_all(&assets).expect("create dist/assets");
    write_if_missing(
        &dist.join("index.html"),
        "<!doctype html><meta charset=utf-8><title>tellur-live</title>\
         <body><p>web bundle missing; rebuild without TELLUR_SKIP_WEB_BUILD.</p></body>",
    );
    write_if_missing(&assets.join("index.js"), "");
    write_if_missing(&assets.join("index.css"), "");
}

fn write_if_missing(path: &Path, contents: &str) {
    if path.exists() {
        return;
    }
    std::fs::write(path, contents).unwrap_or_else(|e| {
        panic!("failed to write placeholder {}: {e}", path.display())
    });
}

fn build_web(web_dir: &Path) {
    if !web_dir.join("node_modules").exists() {
        run("npm", &["install", "--no-audit", "--no-fund"], web_dir);
    }
    run("npm", &["run", "build"], web_dir);
}

fn run(program: &str, args: &[&str], cwd: &Path) {
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|e| {
            panic!(
                "failed to spawn {program} {}: {e}. \
                 Install node/npm or set TELLUR_SKIP_WEB_BUILD=1 to skip.",
                args.join(" ")
            )
        });
    if !status.success() {
        panic!("{program} {} failed with status {status}", args.join(" "));
    }
}

fn track_dir(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            track_dir(&path);
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
