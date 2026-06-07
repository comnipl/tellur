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
use std::fs;
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
    /// Scaffold a new timeline project as a member of the current workspace.
    Create(CreateArgs),
    /// Build a timeline project's cdylib and serve its live preview.
    Live(LiveArgs),
}

#[derive(Args)]
struct CreateArgs {
    /// Name of the new timeline project (a valid crate name).
    name: String,
    /// Display title for the timeline (defaults to the project name).
    #[arg(long)]
    title: Option<String>,
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
        Command::Create(args) => create(args),
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

fn create(args: CreateArgs) -> Result<(), Box<dyn Error>> {
    validate_crate_name(&args.name)?;
    let title = args.title.unwrap_or_else(|| args.name.clone());

    let metadata = MetadataCommand::new().exec()?;
    let workspace_root = metadata.workspace_root.as_std_path();

    let project_dir = env::current_dir()?.join(&args.name);
    if project_dir.exists() {
        return Err(format!("`{}` already exists", project_dir.display()).into());
    }
    let member = relative_to(workspace_root, &project_dir).ok_or_else(|| {
        format!(
            "create the project inside the workspace at {}",
            workspace_root.display()
        )
    })?;

    fs::create_dir_all(project_dir.join("src"))?;
    fs::write(project_dir.join("Cargo.toml"), project_manifest(&args.name))?;
    fs::write(project_dir.join("src/lib.rs"), starter_scene(&title))?;

    // If `tellur` is itself a member of this workspace, point the new project at
    // it by path; otherwise leave a version requirement for the user to pin.
    let tellur_path = metadata
        .packages
        .iter()
        .find(|package| package.name == "tellur")
        .and_then(|package| package.manifest_path.parent())
        .and_then(|dir| relative_to(workspace_root, dir.as_std_path()));
    register_member(workspace_root, &member, tellur_path.as_deref())?;

    println!("created {}", project_dir.display());
    println!("  cd {} && tellur live", args.name);
    Ok(())
}

fn validate_crate_name(name: &str) -> Result<(), Box<dyn Error>> {
    let valid = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    valid
        .then_some(())
        .ok_or_else(|| format!("`{name}` is not a valid crate name").into())
}

/// `target` expressed relative to `base` with `/` separators, or `None` when
/// `target` is not inside `base`.
fn relative_to(base: &Path, target: &Path) -> Option<String> {
    let rel = target.strip_prefix(base).ok()?;
    Some(
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

fn project_manifest(name: &str) -> String {
    format!(
        "[package]\n\
         name = \"{name}\"\n\
         version = \"0.1.0\"\n\
         edition = \"2021\"\n\
         publish = false\n\
         \n\
         [lib]\n\
         crate-type = [\"cdylib\"]\n\
         \n\
         [dependencies]\n\
         tellur = {{ workspace = true }}\n"
    )
}

fn starter_scene(title: &str) -> String {
    format!(
        r##"//! A starter tellur timeline. Edit `build` to design your scene, then run
//! `tellur live` in this directory to preview it.

use tellur::core::geometry::{{Constraints, Vec2}};
use tellur::core::raster::{{PixelFormat, RasterComponent, RasterImage, Resolution}};
use tellur::core::render_context::RenderContext;
use tellur::core::timeline_component::Timed;
use tellur::core::timeline_container::Timeline;
use tellur::prelude::{{component, Keyable}};

/// An opaque square. Replace this with your own components — see the tellur docs
/// for shapes, text, effects, and `#[component(timeline)]` with `#[clock]` for
/// animation.
#[component(raster)]
#[derive(Clone, Keyable)]
pub struct Block {{
    pub size: f32,
}}

impl RasterComponent for Block {{
    fn layout(&self, constraints: Constraints) -> Vec2 {{
        constraints.constrain(Vec2(self.size, self.size))
    }}

    fn render(&self, size: Vec2, _target: Resolution, _ctx: &mut dyn RenderContext) -> RasterImage {{
        let w = (size.0 as u32).max(1);
        let h = (size.1 as u32).max(1);
        // Opaque white pixels (RGBA).
        RasterImage::cpu(w, h, PixelFormat::Rgba8, vec![255u8; (w * h * 4) as usize])
    }}
}}

fn build() -> Timeline {{
    Timeline::builder()
        .child(Block::builder().size(200.0).build().at(0.0..3.0))
        .build()
}}

tellur::export_timeline!("main", "{title}", build, canvas = (1920.0, 1080.0));
"##
    )
}

/// Registers `member` in the workspace root's `[workspace].members` and ensures
/// `[workspace.dependencies].tellur` exists, preserving the file's formatting.
fn register_member(
    workspace_root: &Path,
    member: &str,
    tellur_path: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let manifest_path = workspace_root.join("Cargo.toml");
    let mut doc = fs::read_to_string(&manifest_path)?.parse::<toml_edit::DocumentMut>()?;

    let workspace = doc["workspace"]
        .as_table_mut()
        .ok_or("workspace root Cargo.toml has no [workspace] table")?;

    let members = workspace
        .entry("members")
        .or_insert(toml_edit::value(toml_edit::Array::new()))
        .as_array_mut()
        .ok_or("workspace.members is not an array")?;
    if !members.iter().any(|value| value.as_str() == Some(member)) {
        members.push(member);
    }

    let dependencies = workspace
        .entry("dependencies")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or("workspace.dependencies is not a table")?;
    if !dependencies.contains_key("tellur") {
        let value = match tellur_path {
            Some(path) => {
                let mut table = toml_edit::InlineTable::new();
                table.insert("path", path.into());
                toml_edit::value(table)
            }
            None => toml_edit::value("0.1"),
        };
        dependencies.insert("tellur", value);
    }

    fs::write(&manifest_path, doc.to_string())?;
    Ok(())
}
