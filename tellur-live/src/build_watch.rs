use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct AutoBuildOptions {
    pub package: Option<String>,
    pub example: String,
    pub manifest_path: Option<PathBuf>,
    pub watch_paths: Vec<PathBuf>,
    pub poll_interval: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileStatus {
    Compiled,
    Compiling,
    Failed,
}

impl CompileStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compiled => "compiled",
            Self::Compiling => "compiling",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompileSnapshot {
    pub status: CompileStatus,
    pub last_error: Option<String>,
}

impl CompileSnapshot {
    pub fn compiled() -> Self {
        Self {
            status: CompileStatus::Compiled,
            last_error: None,
        }
    }
}

#[derive(Clone)]
pub struct CompileState {
    inner: Arc<Mutex<CompileSnapshot>>,
}

impl CompileState {
    pub fn compiled() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CompileSnapshot::compiled())),
        }
    }

    pub fn snapshot(&self) -> CompileSnapshot {
        self.inner
            .lock()
            .map(|state| state.clone())
            .unwrap_or_else(|_| CompileSnapshot {
                status: CompileStatus::Failed,
                last_error: Some("compile state lock poisoned".to_owned()),
            })
    }

    fn set(&self, status: CompileStatus, last_error: Option<String>) {
        if let Ok(mut state) = self.inner.lock() {
            *state = CompileSnapshot { status, last_error };
        }
    }
}

pub fn start_build_watcher(options: AutoBuildOptions) -> CompileState {
    let state = CompileState::compiled();
    let state_for_thread = state.clone();
    thread::spawn(move || watch_loop(options, state_for_thread));
    state
}

pub fn run_release_build_once(options: &AutoBuildOptions) -> Result<(), String> {
    run_release_build(options)
}

fn watch_loop(options: AutoBuildOptions, state: CompileState) {
    let watch_paths = if options.watch_paths.is_empty() {
        vec![std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))]
    } else {
        options.watch_paths.clone()
    };
    let poll_interval = options.poll_interval.max(Duration::from_millis(200));
    eprintln!("watching {} source path(s):", watch_paths.len());
    for path in &watch_paths {
        eprintln!("  {}", path.display());
    }
    let mut last_seen = scan_paths(&watch_paths).ok();

    loop {
        thread::sleep(poll_interval);
        let fingerprint = match scan_paths(&watch_paths) {
            Ok(fingerprint) => fingerprint,
            Err(e) => {
                state.set(
                    CompileStatus::Failed,
                    Some(format!("failed to scan watched source files: {e}")),
                );
                continue;
            }
        };
        if last_seen == Some(fingerprint) {
            continue;
        }

        state.set(CompileStatus::Compiling, None);
        eprintln!("source change detected; running release build");
        match run_release_build(&options) {
            Ok(()) => {
                eprintln!("release build finished");
                state.set(CompileStatus::Compiled, None)
            }
            Err(e) => {
                eprintln!("release build failed: {e}");
                state.set(CompileStatus::Failed, Some(e))
            }
        }
        last_seen = Some(fingerprint);
    }
}

fn run_release_build(options: &AutoBuildOptions) -> Result<(), String> {
    let mut command = Command::new("cargo");
    command.arg("build").arg("--release");
    if let Some(manifest_path) = &options.manifest_path {
        command.arg("--manifest-path").arg(manifest_path);
    }
    if let Some(package) = &options.package {
        command.arg("-p").arg(package);
    }
    command.arg("--example").arg(&options.example);

    let description = describe_command(&command);
    eprintln!("{description}");
    let status = command
        .status()
        .map_err(|e| format!("failed to run {description}: {e}"))?;
    if status.success() {
        return Ok(());
    }

    Err(format!(
        "{description} failed with {status}; see tellur-live stderr for diagnostics"
    ))
}

fn describe_command(command: &Command) -> String {
    let mut parts = vec![command.get_program().to_string_lossy().into_owned()];
    parts.extend(
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned()),
    );
    parts.join(" ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WatchFingerprint {
    files: u64,
    len: u64,
    hash: u64,
}

impl Default for WatchFingerprint {
    fn default() -> Self {
        Self {
            files: 0,
            len: 0,
            hash: FNV_OFFSET,
        }
    }
}

fn scan_paths(paths: &[PathBuf]) -> io::Result<WatchFingerprint> {
    let mut fingerprint = WatchFingerprint::default();
    for path in paths {
        scan_path(path, &mut fingerprint)?;
    }
    Ok(fingerprint)
}

fn scan_path(path: &Path, fingerprint: &mut WatchFingerprint) -> io::Result<()> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    if metadata.is_dir() {
        if is_ignored_dir(path) {
            return Ok(());
        }
        let mut entries = fs::read_dir(path)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            scan_path(&entry, fingerprint)?;
        }
        return Ok(());
    }

    if !metadata.is_file() {
        return Ok(());
    }

    fingerprint.files = fingerprint.files.wrapping_add(1);
    fingerprint.len = fingerprint.len.wrapping_add(metadata.len());
    mix_path(&mut fingerprint.hash, path);
    mix_u64(&mut fingerprint.hash, metadata.len());
    mix_u128(&mut fingerprint.hash, modified_nanos(&metadata));
    Ok(())
}

fn is_ignored_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|s| s.to_str()),
        Some("target" | ".git" | "node_modules" | "dist" | ".direnv")
    )
}

fn modified_nanos(metadata: &fs::Metadata) -> u128 {
    metadata
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn mix_path(hash: &mut u64, path: &Path) {
    for byte in path.to_string_lossy().as_bytes() {
        mix_byte(hash, *byte);
    }
}

fn mix_u64(hash: &mut u64, value: u64) {
    for byte in value.to_le_bytes() {
        mix_byte(hash, byte);
    }
}

fn mix_u128(hash: &mut u64, value: u128) {
    for byte in value.to_le_bytes() {
        mix_byte(hash, byte);
    }
}

fn mix_byte(hash: &mut u64, byte: u8) {
    *hash ^= u64::from(byte);
    *hash = hash.wrapping_mul(FNV_PRIME);
}

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
