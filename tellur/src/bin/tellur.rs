//! The `tellur` command.
//!
//! `tellur live` resolves the target timeline project via `cargo metadata`,
//! (re)builds its `cdylib`, and serves the live preview through `tellur-live`,
//! hot-reloading on source changes.
//!
//! It is a rustup-style version dispatcher. On the **fast path** — when the
//! project pins the same `tellur` version as this binary — the installed `tellur`
//! is itself the host and serves in-process. On the **slow path** — when the
//! project pins a different version — the in-process host would mismatch the
//! plugin's Rust ABI, so it `cargo install`s a host from the project's exact
//! `tellur` source (path / crates.io / git), caches it keyed by source + rustc,
//! and hands off to it (`TELLUR_HOST` guards against re-dispatching).

use std::collections::hash_map::DefaultHasher;
use std::env;
use std::error::Error;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command as Process;
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

    // Version dispatch (slow path): if the project pins a different `tellur`
    // version than this binary, the in-process host would mismatch the plugin's
    // ABI. Build (and cache) a host matched to the project's exact `tellur` and
    // hand off to it. `TELLUR_HOST` guards against re-dispatching after handoff.
    if env::var_os("TELLUR_HOST").is_none() {
        if let Some(tellur) = project_tellur_package(&metadata, package) {
            let version = tellur.version.to_string();
            if version != env!("CARGO_PKG_VERSION") {
                let source = tellur_source(tellur)?;
                return dispatch_to_host(&source, &version);
            }
        }
    }

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
            .iter()
            .find(|package| package.name == name)
            .copied()
            .ok_or_else(|| {
                format!(
                    "no workspace member named `{name}`; members: {}",
                    member_names(&members)
                )
                .into()
            });
    }

    // The deepest member directory that contains the cwd wins (nested members).
    let cwd = env::current_dir()?;
    members
        .iter()
        .filter_map(|package| {
            let dir = package.manifest_path.parent()?.as_std_path();
            cwd.starts_with(dir)
                .then(|| (*package, dir.components().count()))
        })
        .max_by_key(|(_, depth)| *depth)
        .map(|(package, _)| package)
        .ok_or_else(|| {
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
        // Only the `lib` target produces the plugin `cdylib`; an `[[example]]`
        // declared with `crate-type = ["cdylib"]` also carries `cdylib` in
        // `crate_types` but has `kind = ["example"]`, so match on the lib kind.
        .find(|target| target.is_lib() && target.crate_types.iter().any(|kind| kind == "cdylib"))
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
    let (w, h): (u32, u32) = (w.trim().parse()?, h.trim().parse()?);
    if w == 0 || h == 0 {
        return Err("resolution width and height must be non-zero".into());
    }
    Ok(Resolution::new(w, h))
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
        RasterImage::cpu(
            w,
            h,
            PixelFormat::Rgba8,
            vec![255u8; w as usize * h as usize * 4],
        )
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

/// The resolved `tellur` package the target project depends on, via the lock
/// graph — so its version/source reflects what the project actually builds with.
fn project_tellur_package<'a>(metadata: &'a Metadata, package: &Package) -> Option<&'a Package> {
    let node = metadata
        .resolve
        .as_ref()?
        .nodes
        .iter()
        .find(|node| node.id == package.id)?;
    node.deps.iter().find_map(|dep| {
        let pkg = metadata.packages.iter().find(|pkg| pkg.id == dep.pkg)?;
        (pkg.name == "tellur").then_some(pkg)
    })
}

/// Where a project's `tellur` dependency resolves from — enough to rebuild a
/// byte-identical host with `cargo install`.
#[derive(Debug, PartialEq)]
enum TellurSource {
    Path(PathBuf),
    CratesIo { version: String },
    Git { url: String, rev: String },
}

fn tellur_source(package: &Package) -> Result<TellurSource, Box<dyn Error>> {
    match &package.source {
        // No source id → a path dependency (or workspace member).
        None => {
            let dir = package
                .manifest_path
                .parent()
                .ok_or("the tellur path dependency has no directory")?
                .as_std_path()
                .to_path_buf();
            Ok(TellurSource::Path(dir))
        }
        Some(source) if source.is_crates_io() => Ok(TellurSource::CratesIo {
            version: package.version.to_string(),
        }),
        Some(source) => parse_git_source(&source.repr),
    }
}

/// Parses a cargo git source id (`git+<url>[?query]#<locked-sha>`).
fn parse_git_source(repr: &str) -> Result<TellurSource, Box<dyn Error>> {
    let rest = repr
        .strip_prefix("git+")
        .ok_or_else(|| format!("unsupported tellur source for the slow path: {repr}"))?;
    let (locator, rev) = rest
        .rsplit_once('#')
        .ok_or_else(|| format!("git tellur source has no locked commit: {repr}"))?;
    let url = locator.split('?').next().unwrap_or(locator);
    Ok(TellurSource::Git {
        url: url.to_owned(),
        rev: rev.to_owned(),
    })
}

impl TellurSource {
    /// The `cargo install` arguments that select this exact source.
    fn install_args(&self) -> Vec<String> {
        match self {
            TellurSource::Path(dir) => vec!["--path".to_owned(), dir.display().to_string()],
            TellurSource::CratesIo { version } => {
                vec![
                    "tellur".to_owned(),
                    "--version".to_owned(),
                    format!("={version}"),
                ]
            }
            TellurSource::Git { url, rev } => vec![
                "--git".to_owned(),
                url.clone(),
                "--rev".to_owned(),
                rev.clone(),
                "tellur".to_owned(),
            ],
        }
    }

    /// A stable string identifying this source for the host cache key.
    fn discriminant(&self) -> String {
        match self {
            TellurSource::Path(dir) => format!("path:{}", dir.display()),
            TellurSource::CratesIo { version } => format!("cratesio:{version}"),
            TellurSource::Git { url, rev } => format!("git:{url}#{rev}"),
        }
    }
}

/// Builds (if needed) and hands off to a host matched to `source`/`version`.
fn dispatch_to_host(source: &TellurSource, version: &str) -> Result<(), Box<dyn Error>> {
    let rustc = rustc_fingerprint()?;
    let key = host_cache_key(version, &source.discriminant(), &rustc);
    let cache_root = host_cache_dir()?.join(&key);
    let host_bin = cache_root.join("bin").join(host_bin_name());

    if !host_bin.exists() {
        eprintln!(
            "tellur {version}: building a version-matched host (first run for this version)…"
        );
        install_host(source, &cache_root)?;
    }

    eprintln!("tellur {version}: handing off to {}", host_bin.display());
    let status = Process::new(&host_bin)
        .args(env::args_os().skip(1))
        .env("TELLUR_HOST", "1")
        .status()
        .map_err(|e| format!("failed to run the version-matched host: {e}"))?;
    std::process::exit(status.code().unwrap_or(1));
}

fn install_host(source: &TellurSource, cache_root: &Path) -> Result<(), Box<dyn Error>> {
    let status = Process::new("cargo")
        .arg("install")
        .args(source.install_args())
        .arg("--bin")
        .arg("tellur")
        .arg("--features")
        .arg("cli")
        .arg("--root")
        .arg(cache_root)
        .status()
        .map_err(|e| format!("failed to run cargo install: {e}"))?;
    if !status.success() {
        return Err(format!("cargo install for the tellur host failed with {status}").into());
    }
    Ok(())
}

/// The first line of `rustc -vV` — identifies the toolchain (version + commit +
/// host triple) so a host built with a different compiler gets a distinct cache
/// entry (the cdylib ABI depends on the compiler, not just the tellur version).
fn rustc_fingerprint() -> Result<String, Box<dyn Error>> {
    let output = Process::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|e| format!("failed to run rustc -vV: {e}"))?;
    if !output.status.success() {
        return Err(format!("rustc -vV failed with {}", output.status).into());
    }
    // An empty fingerprint would collapse distinct toolchains onto one cache
    // entry, so a stale host could be handed off for a mismatched ABI — fail
    // loudly instead.
    let fingerprint = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if fingerprint.is_empty() {
        return Err("rustc -vV produced no version line to fingerprint the toolchain".into());
    }
    Ok(fingerprint)
}

fn host_cache_dir() -> Result<PathBuf, Box<dyn Error>> {
    if let Some(dir) = env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(dir).join("tellur").join("hosts"));
    }
    let home = env::var_os("HOME").ok_or("neither XDG_CACHE_HOME nor HOME is set")?;
    Ok(PathBuf::from(home)
        .join(".cache")
        .join("tellur")
        .join("hosts"))
}

fn host_cache_key(version: &str, discriminant: &str, rustc: &str) -> String {
    let mut hasher = DefaultHasher::new();
    discriminant.hash(&mut hasher);
    rustc.hash(&mut hasher);
    let sanitized: String = version
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{sanitized}-{:016x}", hasher.finish())
}

fn host_bin_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "tellur.exe"
    } else {
        "tellur"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_source_with_query_and_rev() {
        assert_eq!(
            parse_git_source("git+https://github.com/comnipl/tellur?rev=abc#deadbeef").unwrap(),
            TellurSource::Git {
                url: "https://github.com/comnipl/tellur".to_owned(),
                rev: "deadbeef".to_owned(),
            }
        );
    }

    #[test]
    fn parses_git_source_without_query() {
        assert_eq!(
            parse_git_source("git+https://github.com/comnipl/tellur#deadbeef").unwrap(),
            TellurSource::Git {
                url: "https://github.com/comnipl/tellur".to_owned(),
                rev: "deadbeef".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_non_git_repr() {
        assert!(parse_git_source("registry+https://github.com/rust-lang/crates.io-index").is_err());
    }

    #[test]
    fn install_args_select_the_source() {
        assert_eq!(
            TellurSource::CratesIo {
                version: "0.4.0".to_owned()
            }
            .install_args(),
            vec!["tellur", "--version", "=0.4.0"]
        );
        assert_eq!(
            TellurSource::Git {
                url: "u".to_owned(),
                rev: "r".to_owned()
            }
            .install_args(),
            vec!["--git", "u", "--rev", "r", "tellur"]
        );
        assert_eq!(
            TellurSource::Path(PathBuf::from("/a/b")).install_args(),
            vec!["--path", "/a/b"]
        );
    }

    #[test]
    fn cache_key_is_stable_and_discriminates() {
        let base = host_cache_key("0.4.0", "git:u#r", "rustc 1.95.0");
        assert_eq!(base, host_cache_key("0.4.0", "git:u#r", "rustc 1.95.0"));
        assert!(base.starts_with("0.4.0-"));
        // A different toolchain or source must yield a different cache entry.
        assert_ne!(base, host_cache_key("0.4.0", "git:u#r", "rustc 1.96.0"));
        assert_ne!(base, host_cache_key("0.4.0", "path:/x", "rustc 1.95.0"));
    }
}
