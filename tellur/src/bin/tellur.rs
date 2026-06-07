//! The `tellur` command.
//!
//! `tellur live` resolves the target timeline project via `cargo metadata`,
//! (re)builds its `cdylib`, and serves the live preview through `tellur-live`,
//! hot-reloading on source changes.
//!
//! This binary is the future home of the rustup-style version dispatcher
//! (running a host built against the project's exact `tellur` version). For now
//! it takes the fast path: the installed `tellur` is itself the host, which is
//! correct whenever the project and the CLI share one `tellur` version — the
//! common case when a workspace pins a single version.

use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cargo_metadata::{Metadata, MetadataCommand, Package};
use clap::{Args, Parser, Subcommand};

use tellur::core::raster::Resolution;
use tellur::core::render_context::GpuPreference;
use tellur_live::{run_build_once, serve, AutoBuildOptions, ServerOptions};

#[derive(Parser)]
#[command(
    name = "tellur",
    version,
    about = "Author and preview tellur timelines"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build a timeline project's cdylib and serve its live preview.
    Live(LiveArgs),
}

#[derive(Args)]
struct LiveArgs {
    /// Workspace member to preview. Defaults to the package containing the
    /// current directory.
    #[arg(short = 'p', long = "project")]
    project: Option<String>,
    /// Host address to bind the preview server to.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// TCP port for the preview server.
    #[arg(long, default_value_t = 4317)]
    port: u16,
    /// Preview render resolution, `WIDTHxHEIGHT`.
    #[arg(long, default_value = "1280x720")]
    size: String,
    /// Preview frame rate.
    #[arg(long, default_value_t = 30)]
    fps: u32,
    /// Prefer GPU rendering.
    #[arg(long, conflicts_with = "no_gpu")]
    gpu: bool,
    /// Disable GPU rendering.
    #[arg(long)]
    no_gpu: bool,
    /// Build in debug mode (faster rebuilds) instead of release.
    #[arg(long)]
    debug: bool,
    /// Serve the current build without watching/rebuilding on changes.
    #[arg(long)]
    no_watch: bool,
    /// Print per-frame timing / cache diagnostics.
    #[arg(long)]
    verbose: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    match Cli::parse().command {
        Command::Live(args) => live(args),
    }
}

fn live(args: LiveArgs) -> Result<(), Box<dyn Error>> {
    let resolution = parse_resolution(&args.size)?;
    let gpu_preference = if args.no_gpu {
        GpuPreference::Disabled
    } else if args.gpu {
        GpuPreference::PreferGpu
    } else {
        GpuPreference::Auto
    };
    let release = !args.debug;

    let metadata = MetadataCommand::new().exec()?;
    let package = resolve_project(&metadata, args.project.as_deref())?;
    let lib_name = cdylib_target_name(package).ok_or_else(|| {
        format!(
            "`{}` is not a tellur timeline project \
             (its library target needs `crate-type = [\"cdylib\"]`)",
            package.name
        )
    })?;

    let profile_dir = if release { "release" } else { "debug" };
    let plugin_path = metadata
        .target_directory
        .as_std_path()
        .join(profile_dir)
        .join(dynamic_library_file_name(&lib_name));

    let package_dir = package
        .manifest_path
        .parent()
        .map(|dir| dir.as_std_path().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let workspace_root = metadata.workspace_root.as_std_path();

    let build_options = AutoBuildOptions {
        package: Some(package.name.clone()),
        // `None` builds the package's cdylib library, not an example.
        example: None,
        release,
        manifest_path: None,
        watch_paths: watch_paths(&package_dir, workspace_root),
        poll_interval: Duration::from_millis(250),
    };

    // Always build once so the cdylib exists; `--no-watch` only suppresses the
    // rebuild-on-change watcher (when watching, `serve` runs the initial build).
    let auto_build = if args.no_watch {
        run_build_once(&build_options).map_err(|e| -> Box<dyn Error> { e.into() })?;
        None
    } else {
        Some(build_options)
    };

    serve(ServerOptions {
        plugin_path,
        bind: format!("{}:{}", args.host, args.port),
        resolution,
        fps: args.fps,
        gpu_preference,
        verbose: args.verbose,
        auto_build,
    })
}

/// Resolves the workspace member to preview: the named one, or — with no `name`
/// — the member whose directory contains the current directory.
fn resolve_project<'a>(
    metadata: &'a Metadata,
    name: Option<&str>,
) -> Result<&'a Package, Box<dyn Error>> {
    let members = metadata.workspace_packages();

    if let Some(name) = name {
        return members
            .into_iter()
            .find(|package| package.name == name)
            .ok_or_else(|| {
                format!(
                    "no workspace member named `{name}`; members: {}",
                    member_names(&metadata.workspace_packages())
                )
                .into()
            });
    }

    let cwd = env::current_dir()?;
    let mut best: Option<(&Package, usize)> = None;
    for package in &members {
        let Some(dir) = package.manifest_path.parent() else {
            continue;
        };
        let dir = dir.as_std_path();
        if cwd.starts_with(dir) {
            let depth = dir.components().count();
            if best.map(|(_, d)| depth > d).unwrap_or(true) {
                best = Some((package, depth));
            }
        }
    }
    best.map(|(package, _)| package).ok_or_else(|| {
        format!(
            "could not infer the project from the current directory; pass --project <name>. \
             members: {}",
            member_names(&members)
        )
        .into()
    })
}

/// The name of the package's `cdylib` library target, if it has one.
fn cdylib_target_name(package: &Package) -> Option<String> {
    package
        .targets
        .iter()
        .find(|target| target.crate_types.iter().any(|kind| kind == "cdylib"))
        .map(|target| target.name.clone())
}

fn member_names(members: &[&Package]) -> String {
    members
        .iter()
        .map(|package| package.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn watch_paths(package_dir: &Path, workspace_root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    push_if_exists(&mut paths, package_dir.join("src"));
    push_if_exists(&mut paths, package_dir.join("Cargo.toml"));
    push_if_exists(&mut paths, workspace_root.join("Cargo.toml"));
    push_if_exists(&mut paths, workspace_root.join("Cargo.lock"));
    paths
}

fn push_if_exists(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if path.exists() {
        paths.push(path);
    }
}

fn dynamic_library_file_name(lib_name: &str) -> String {
    let lib_name = lib_name.replace('-', "_");
    if cfg!(target_os = "windows") {
        format!("{lib_name}.dll")
    } else if cfg!(target_os = "macos") {
        format!("lib{lib_name}.dylib")
    } else {
        format!("lib{lib_name}.so")
    }
}

fn parse_resolution(s: &str) -> Result<Resolution, Box<dyn Error>> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or("resolution must be WIDTHxHEIGHT")?;
    Ok(Resolution::new(w.trim().parse()?, h.trim().parse()?))
}
