//! Rust-internal timeline plugin ABI and hot-reload loader.
//!
//! This module intentionally uses Rust types across the dynamic-library
//! boundary. That is suitable for local editing when the host and plugin are
//! built from the same workspace/toolchain; it is not a stable C ABI.

use std::error::Error;
use std::ffi::{CStr, CString};
use std::fmt;
use std::fs;
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tellur_core::raster::{RasterImage, Resolution};
use tellur_core::render_context::RenderContext;
use tellur_core::time::TimelineTime;
use tellur_core::timeline::Timeline;

pub const ENTRY_SYMBOL: &[u8] = b"tellur_timeline_collection_v1\0";

type EntryFn = unsafe extern "Rust" fn() -> Box<dyn TimelineCollection>;

/// Metadata for one timeline exposed by a plugin.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineInfo {
    pub id: String,
    pub title: String,
    pub duration: f32,
}

/// A collection of timelines exported by one dynamic library.
pub trait TimelineCollection: Send {
    fn timelines(&self) -> Vec<TimelineInfo>;

    fn build(
        &self,
        id: &str,
        t: TimelineTime,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage>;
}

/// Wraps a single [`Timeline`] as a one-entry collection.
pub struct SingleTimeline<T: Timeline + Send> {
    id: &'static str,
    title: &'static str,
    timeline: T,
}

pub fn single_timeline<T: Timeline + Send>(
    id: &'static str,
    title: &'static str,
    timeline: T,
) -> SingleTimeline<T> {
    SingleTimeline {
        id,
        title,
        timeline,
    }
}

impl<T: Timeline + Send> TimelineCollection for SingleTimeline<T> {
    fn timelines(&self) -> Vec<TimelineInfo> {
        vec![TimelineInfo {
            id: self.id.to_owned(),
            title: self.title.to_owned(),
            duration: self.timeline.duration(),
        }]
    }

    fn build(
        &self,
        id: &str,
        t: TimelineTime,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        (id == self.id).then(|| self.timeline.build(t, target, ctx))
    }
}

/// Exports a single timeline builder from a `cdylib`.
///
/// ```ignore
/// fn build() -> impl tellur_core::timeline::Timeline { ... }
/// tellur_live::export_timeline!("main", "Main", build);
/// ```
#[macro_export]
macro_rules! export_timeline {
    ($id:expr, $title:expr, $builder:path) => {
        #[no_mangle]
        pub extern "Rust" fn tellur_timeline_collection_v1(
        ) -> ::std::boxed::Box<dyn $crate::TimelineCollection> {
            ::std::boxed::Box::new($crate::single_timeline($id, $title, $builder()))
        }
    };
}

/// Exports a custom [`TimelineCollection`] builder from a `cdylib`.
#[macro_export]
macro_rules! export_timeline_collection {
    ($builder:path) => {
        #[no_mangle]
        pub extern "Rust" fn tellur_timeline_collection_v1(
        ) -> ::std::boxed::Box<dyn $crate::TimelineCollection> {
            ::std::boxed::Box::new($builder())
        }
    };
}

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
}

struct LoadedPlugin {
    stamp: SourceStamp,
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
}

impl HotReloadPlugin {
    pub fn new(source_path: impl Into<PathBuf>) -> Self {
        Self {
            source_path: source_path.into(),
            loaded: None,
            retired_libraries: Vec::new(),
            last_error: None,
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

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn reload_if_changed(&mut self) -> Result<bool, PluginLoadError> {
        let stamp = source_stamp(&self.source_path)?;
        let changed = self
            .loaded
            .as_ref()
            .map(|loaded| loaded.stamp != stamp)
            .unwrap_or(true);

        if !changed {
            return Ok(false);
        }

        match load_plugin(&self.source_path, stamp) {
            Ok(next) => {
                if let Some(previous) = self.loaded.replace(next) {
                    drop(previous.collection);
                    self.retired_libraries.push(previous.library);
                }
                self.last_error = None;
                Ok(true)
            }
            Err(e) if self.loaded.is_some() => {
                self.last_error = Some(e.to_string());
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

fn source_stamp(path: &Path) -> Result<SourceStamp, PluginLoadError> {
    let metadata = fs::metadata(path)?;
    Ok(SourceStamp {
        modified: metadata.modified()?,
        len: metadata.len(),
    })
}

fn load_plugin(path: &Path, stamp: SourceStamp) -> Result<LoadedPlugin, PluginLoadError> {
    let staged_path = stage_library(path, stamp)?;
    let library = unsafe { DynamicLibrary::open(&staged_path)? };
    let entry: EntryFn = unsafe { library.symbol(ENTRY_SYMBOL)? };
    let collection = unsafe { entry() };
    Ok(LoadedPlugin {
        stamp,
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
    let staged = dir.join(format!("{stem}-{modified}-{}.{}", stamp.len, ext));
    fs::copy(path, &staged)?;
    Ok(staged)
}

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
