use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tellur_core::raster::Resolution;
use tellur_core::render_context::GpuPreference;
use tellur_live::{serve, AutoBuildOptions, ServerOptions};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help") {
        println!("{}", usage());
        return Ok(());
    }
    let options = parse_args(args.into_iter())?;
    serve(options)
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<ServerOptions, Box<dyn Error>> {
    let Some(command) = args.next() else {
        return Err(usage().into());
    };
    if command != "serve" {
        return Err(usage().into());
    }

    let mut plugin_path: Option<PathBuf> = None;
    let mut bind: Option<String> = None;
    let mut host = "127.0.0.1".to_owned();
    let mut port = 4317u16;
    let mut resolution = Resolution::new(1280, 720);
    let mut fps = 30u32;
    let mut gpu_preference = GpuPreference::Auto;
    let mut verbose = false;
    let mut auto_build_requested = false;
    let mut build_package: Option<String> = None;
    let mut build_example: Option<String> = None;
    let mut build_manifest: Option<PathBuf> = None;
    let mut watch_paths: Vec<PathBuf> = Vec::new();
    let mut explicit_watch_paths = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--plugin" => {
                plugin_path = Some(PathBuf::from(
                    args.next().ok_or("--plugin requires a path")?,
                ));
            }
            "--bind" => {
                bind = Some(args.next().ok_or("--bind requires an address")?);
            }
            "--host" => {
                host = args.next().ok_or("--host requires an address")?;
            }
            "--port" => {
                port = args
                    .next()
                    .ok_or("--port requires a value")?
                    .parse()
                    .map_err(|_| "--port must be a TCP port")?;
            }
            "--size" => {
                resolution = parse_resolution(&args.next().ok_or("--size requires WxH")?)?;
            }
            "--fps" => {
                fps = args
                    .next()
                    .ok_or("--fps requires a value")?
                    .parse()
                    .map_err(|_| "--fps must be a positive integer")?;
                if fps == 0 {
                    return Err("--fps must be greater than zero".into());
                }
            }
            "--verbose" => {
                verbose = true;
            }
            "--gpu" => {
                gpu_preference = GpuPreference::PreferGpu;
            }
            "--no-gpu" => {
                gpu_preference = GpuPreference::Disabled;
            }
            "--watch" => {
                auto_build_requested = true;
            }
            "--watch-path" => {
                auto_build_requested = true;
                explicit_watch_paths = true;
                watch_paths.push(PathBuf::from(
                    args.next().ok_or("--watch-path requires a path")?,
                ));
            }
            "-p" | "--package" | "--build-package" => {
                auto_build_requested = true;
                build_package = Some(args.next().ok_or("--build-package requires a name")?);
            }
            "--example" | "--examples" | "--build-example" => {
                auto_build_requested = true;
                build_example = Some(args.next().ok_or("--build-example requires a name")?);
            }
            "--build-manifest" => {
                auto_build_requested = true;
                build_manifest = Some(PathBuf::from(
                    args.next().ok_or("--build-manifest requires a path")?,
                ));
            }
            "-h" | "--help" => return Err(usage().into()),
            other if plugin_path.is_none() => plugin_path = Some(PathBuf::from(other)),
            other => return Err(format!("unknown argument: {other}\n\n{}", usage()).into()),
        }
    }

    let plugin_path = plugin_path
        .or_else(|| build_example.as_deref().map(infer_plugin_path))
        .ok_or_else(usage)?;
    let project_name = build_package
        .clone()
        .or_else(|| infer_example_name(&plugin_path))
        .unwrap_or_else(|| "Project Name".to_owned());
    let auto_build = if auto_build_requested {
        let example = build_example
            .or_else(|| infer_example_name(&plugin_path))
            .ok_or("--watch requires --build-example when it cannot be inferred from --plugin")?;
        if watch_paths.is_empty() && !explicit_watch_paths {
            watch_paths = infer_watch_paths(
                build_package.as_deref(),
                &example,
                build_manifest.as_deref(),
            );
        }
        Some(AutoBuildOptions {
            package: build_package,
            example: Some(example),
            release: true,
            manifest_path: build_manifest,
            watch_paths,
            poll_interval: Duration::from_millis(250),
        })
    } else {
        None
    };

    Ok(ServerOptions {
        plugin_path,
        project_name,
        bind: bind.unwrap_or_else(|| format!("{host}:{port}")),
        resolution,
        fps,
        gpu_preference,
        verbose,
        auto_build,
    })
}

fn parse_resolution(s: &str) -> Result<Resolution, Box<dyn Error>> {
    let (w, h) = s.split_once('x').ok_or("resolution must be WIDTHxHEIGHT")?;
    Ok(Resolution::new(w.parse()?, h.parse()?))
}

fn infer_example_name(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if cfg!(target_os = "windows") {
        Some(stem.to_owned())
    } else {
        Some(stem.strip_prefix("lib").unwrap_or(stem).to_owned())
    }
}

fn infer_plugin_path(example: &str) -> PathBuf {
    target_root()
        .join("release")
        .join("examples")
        .join(dynamic_library_file_name(example))
}

fn dynamic_library_file_name(example: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{example}.dll")
    } else if cfg!(target_os = "macos") {
        format!("lib{example}.dylib")
    } else {
        format!("lib{example}.so")
    }
}

fn target_root() -> PathBuf {
    if let Some(target_dir) = env::var_os("CARGO_TARGET_DIR") {
        return PathBuf::from(target_dir);
    }
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    find_workspace_root(&cwd).unwrap_or(cwd).join("target")
}

fn infer_watch_paths(
    package: Option<&str>,
    example: &str,
    manifest_path: Option<&Path>,
) -> Vec<PathBuf> {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let workspace_root = find_workspace_root(&cwd);
    let mut paths = Vec::new();

    let package_dir = manifest_path
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .or_else(|| {
            package.and_then(|name| find_workspace_package_dir(name, workspace_root.as_deref()))
        });
    let Some(package_dir) = package_dir else {
        return paths;
    };

    if let Some(root) = &workspace_root {
        push_if_exists(&mut paths, root.join("Cargo.toml"));
        push_if_exists(&mut paths, root.join("Cargo.lock"));
    }

    push_if_exists(&mut paths, package_dir.join("Cargo.toml"));
    push_if_exists(&mut paths, package_dir.join("src"));
    let example_file = package_dir.join("examples").join(format!("{example}.rs"));
    if example_file.exists() {
        paths.push(example_file);
    } else {
        push_if_exists(&mut paths, package_dir.join("examples"));
    }
    collect_path_dependency_watch_paths(&package_dir.join("Cargo.toml"), &mut paths);

    paths
}

fn find_workspace_root(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let manifest = dir.join("Cargo.toml");
        let Ok(contents) = fs::read_to_string(&manifest) else {
            continue;
        };
        if contents.contains("[workspace]") {
            return Some(dir.to_path_buf());
        }
    }
    None
}

fn find_workspace_package_dir(package: &str, workspace_root: Option<&Path>) -> Option<PathBuf> {
    let root = workspace_root?;
    for member in workspace_members(root) {
        let dir = root.join(member);
        if package_name_from_manifest(&dir.join("Cargo.toml")).as_deref() == Some(package) {
            return Some(dir);
        }
    }
    None
}

fn workspace_members(root: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(root.join("Cargo.toml")) else {
        return Vec::new();
    };
    let Some(start) = contents.find("members") else {
        return Vec::new();
    };
    let Some(open) = contents[start..].find('[').map(|i| start + i) else {
        return Vec::new();
    };
    let Some(close) = contents[open..].find(']').map(|i| open + i) else {
        return Vec::new();
    };
    quoted_values(&contents[open + 1..close])
}

fn package_name_from_manifest(manifest: &Path) -> Option<String> {
    let contents = fs::read_to_string(manifest).ok()?;
    let mut in_package = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package && line.starts_with("name") {
            let (_, value) = line.split_once('=')?;
            return first_quoted_value(value);
        }
    }
    None
}

fn collect_path_dependency_watch_paths(manifest: &Path, paths: &mut Vec<PathBuf>) {
    let Some(package_dir) = manifest.parent() else {
        return;
    };
    let Ok(contents) = fs::read_to_string(manifest) else {
        return;
    };
    for dep in path_dependency_values(&contents) {
        let dep_dir = package_dir.join(dep);
        push_if_exists(paths, dep_dir.join("Cargo.toml"));
        push_if_exists(paths, dep_dir.join("src"));
    }
}

fn path_dependency_values(contents: &str) -> Vec<String> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (_, rest) = line.split_once("path")?;
            let (_, value) = rest.split_once('=')?;
            first_quoted_value(value)
        })
        .collect()
}

fn quoted_values(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = value;
    while let Some(next) = first_quoted_value(rest) {
        let Some(start) = rest.find('"') else {
            break;
        };
        let tail = &rest[start + 1..];
        let Some(end) = tail.find('"') else {
            break;
        };
        values.push(next);
        rest = &tail[end + 1..];
    }
    values
}

fn first_quoted_value(value: &str) -> Option<String> {
    let start = value.find('"')?;
    let tail = &value[start + 1..];
    let end = tail.find('"')?;
    Some(tail[..end].to_owned())
}

fn push_if_exists(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if path.exists() {
        paths.push(path);
    }
}

fn usage() -> String {
    "usage: tellur-live serve (--plugin <path-to-cdylib> | -p <package> --example <example>) [--host 127.0.0.1] [--port 4317] [--bind 127.0.0.1:4317] [--fps 30] [--gpu|--no-gpu] [--verbose] [--watch] [--watch-path <path>] [--build-manifest <Cargo.toml>]".to_owned()
}
