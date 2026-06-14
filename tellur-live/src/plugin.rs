//! Hot-reload loader for timeline plugins.
//!
//! Loads the `cdylib` a project compiles to (via `dlopen`), resolves the
//! [`tellur_plugin::ENTRY_SYMBOL`] entry point, and swaps in a fresh
//! [`TimelineCollection`] when the source library changes on disk. The plugin
//! ABI itself — the entry symbol and the collection trait — lives in
//! `tellur-plugin`; this module is only the host side that consumes it.

use std::error::Error;
use std::ffi::{CStr, CString};
use std::fmt;
use std::fs;
use std::io::Read;
use std::os::raw::{c_char, c_int, c_void};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tellur_plugin::{EntryFn, TimelineCollection, ENTRY_SYMBOL};

#[derive(Debug)]
pub enum PluginLoadError {
    Io(std::io::Error),
    InvalidPath(PathBuf),
    Open { path: PathBuf, message: String },
    Symbol { symbol: String, message: String },
    MissingPlugin,
}

impl fmt::Display for PluginLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::InvalidPath(path) => write!(f, "invalid plugin path: {}", path.display()),
            Self::Open { path, message } => {
                write!(f, "failed to open plugin {}: {message}", path.display())
            }
            Self::Symbol { symbol, message } => {
                write!(f, "failed to load symbol {symbol}: {message}")
            }
            Self::MissingPlugin => write!(f, "plugin is not loaded"),
        }
    }
}

impl Error for PluginLoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for PluginLoadError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SourceStamp {
    modified: SystemTime,
    len: u64,
    changed: Option<(i64, i64)>,
    hash: u64,
}

impl SourceStamp {
    fn cache_key(self) -> String {
        format!("{:016x}-{:x}", self.hash, self.len)
    }

    fn same_content(self, other: Self) -> bool {
        self.len == other.len && self.hash == other.hash
    }

    fn same_file_state(self, modified: SystemTime, len: u64, changed: Option<(i64, i64)>) -> bool {
        self.modified == modified && self.len == len && self.changed == changed
    }
}

struct LoadedPlugin {
    stamp: SourceStamp,
    cache_key: String,
    staged_path: PathBuf,
    collection: Box<dyn TimelineCollection>,
    library: DynamicLibrary,
}

/// Maintains one loaded timeline plugin and reloads it when the source
/// library changes on disk.
pub struct HotReloadPlugin {
    source_path: PathBuf,
    loaded: Option<LoadedPlugin>,
    retired_libraries: Vec<DynamicLibrary>,
    last_error: Option<String>,
    failed_stamp: Option<SourceStamp>,
}

impl HotReloadPlugin {
    pub fn new(source_path: impl Into<PathBuf>) -> Self {
        Self {
            source_path: source_path.into(),
            loaded: None,
            retired_libraries: Vec::new(),
            last_error: None,
            failed_stamp: None,
        }
    }

    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub fn staged_path(&self) -> Option<&Path> {
        self.loaded
            .as_ref()
            .map(|loaded| loaded.staged_path.as_path())
    }

    pub fn cache_key(&self) -> Option<&str> {
        self.loaded.as_ref().map(|loaded| loaded.cache_key.as_str())
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn reload_if_changed(&mut self) -> Result<bool, PluginLoadError> {
        let metadata = fs::metadata(&self.source_path)?;
        let modified = metadata.modified()?;
        let len = metadata.len();
        let changed = metadata_change_time(&metadata);
        if let Some(loaded) = &self.loaded {
            if loaded.stamp.same_file_state(modified, len, changed) {
                return Ok(false);
            }
            if self
                .failed_stamp
                .is_some_and(|stamp| stamp.same_file_state(modified, len, changed))
            {
                return Ok(false);
            }
        }

        let stamp = SourceStamp {
            modified,
            len,
            changed,
            hash: file_hash(&self.source_path)?,
        };
        if let Some(loaded) = self.loaded.as_mut() {
            if loaded.stamp.same_content(stamp) {
                loaded.stamp = stamp;
                self.failed_stamp = None;
                self.last_error = None;
                return Ok(false);
            }
        }

        match load_plugin(&self.source_path, stamp) {
            Ok(next) => {
                if let Some(previous) = self.loaded.replace(next) {
                    drop(previous.collection);
                    self.retired_libraries.push(previous.library);
                }
                self.last_error = None;
                self.failed_stamp = None;
                Ok(true)
            }
            Err(e) if self.loaded.is_some() => {
                self.last_error = Some(e.to_string());
                self.failed_stamp = Some(stamp);
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    pub fn collection(&self) -> Result<&dyn TimelineCollection, PluginLoadError> {
        self.loaded
            .as_ref()
            .map(|loaded| loaded.collection.as_ref())
            .ok_or(PluginLoadError::MissingPlugin)
    }
}

fn metadata_change_time(metadata: &fs::Metadata) -> Option<(i64, i64)> {
    #[cfg(unix)]
    {
        Some((metadata.ctime(), metadata.ctime_nsec()))
    }

    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

fn load_plugin(path: &Path, stamp: SourceStamp) -> Result<LoadedPlugin, PluginLoadError> {
    let staged_path = stage_library(path, stamp)?;
    let library = unsafe { DynamicLibrary::open(&staged_path)? };
    let entry: EntryFn = unsafe { library.symbol(ENTRY_SYMBOL)? };
    let collection = unsafe { entry() };
    Ok(LoadedPlugin {
        stamp,
        cache_key: stamp.cache_key(),
        staged_path,
        collection,
        library,
    })
}

fn stage_library(path: &Path, stamp: SourceStamp) -> Result<PathBuf, PluginLoadError> {
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| PluginLoadError::InvalidPath(path.to_owned()))?;
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("so");
    let stem = file_name
        .strip_suffix(&format!(".{ext}"))
        .unwrap_or(file_name);
    let modified = stamp
        .modified
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join("tellur-live");
    fs::create_dir_all(&dir)?;
    let staged = dir.join(format!(
        "{stem}-{modified}-{}-{:016x}.{ext}",
        stamp.len, stamp.hash
    ));
    fs::copy(path, &staged)?;
    Ok(staged)
}

fn file_hash(path: &Path) -> Result<u64, PluginLoadError> {
    let mut file = fs::File::open(path)?;
    let mut hash = FNV_OFFSET;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for byte in &buf[..n] {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    Ok(hash)
}

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

struct DynamicLibrary {
    handle: *mut c_void,
}

unsafe impl Send for DynamicLibrary {}

impl DynamicLibrary {
    unsafe fn open(path: &Path) -> Result<Self, PluginLoadError> {
        let c_path = CString::new(path.as_os_str().to_string_lossy().as_bytes())
            .map_err(|_| PluginLoadError::InvalidPath(path.to_owned()))?;
        clear_dlerror();
        let handle = dlopen(c_path.as_ptr(), RTLD_NOW | RTLD_LOCAL);
        if handle.is_null() {
            return Err(PluginLoadError::Open {
                path: path.to_owned(),
                message: dlerror_message(),
            });
        }
        Ok(Self { handle })
    }

    unsafe fn symbol<T>(&self, symbol: &[u8]) -> Result<T, PluginLoadError> {
        clear_dlerror();
        let ptr = dlsym(self.handle, symbol.as_ptr().cast());
        if ptr.is_null() {
            return Err(PluginLoadError::Symbol {
                symbol: String::from_utf8_lossy(symbol)
                    .trim_end_matches('\0')
                    .to_owned(),
                message: dlerror_message(),
            });
        }
        Ok(std::mem::transmute_copy::<*mut c_void, T>(&ptr))
    }
}

impl Drop for DynamicLibrary {
    fn drop(&mut self) {
        unsafe {
            dlclose(self.handle);
        }
    }
}

const RTLD_NOW: c_int = 2;
const RTLD_LOCAL: c_int = 0;

#[cfg(target_os = "linux")]
#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *const c_char;
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *const c_char;
}

unsafe fn clear_dlerror() {
    let _ = dlerror();
}

unsafe fn dlerror_message() -> String {
    let err = dlerror();
    if err.is_null() {
        "unknown dynamic loader error".to_owned()
    } else {
        CStr::from_ptr(err).to_string_lossy().into_owned()
    }
}
