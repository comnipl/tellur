//! Rust-internal timeline plugin ABI and hot-reload loader.
//!
//! This module intentionally uses Rust types across the dynamic-library
//! boundary. That is suitable for local editing when the host and plugin are
//! built from the same workspace/toolchain; it is not a stable C ABI.

use std::error::Error;
use std::ffi::{CStr, CString};
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tellur_core::raster::{RasterImage, Resolution};
use tellur_core::render_context::RenderContext;
use tellur_core::time::TimelineTime;
use tellur_core::timeline::Timeline;
use tellur_core::timeline_component::{
    resolve, Arrangement, NodeKind, Clock, ResolveError, ResolvedTimeline, TimelineComponent,
};

/// ABI version carried by the entry symbol.
///
/// Bumped to `v2` for the timeline subsystem migration: the collection now
/// carries the new [`TimelineComponent`] model and gained an `arrangement`
/// vtable slot. The loader resolves this symbol with an unchecked
/// `transmute_copy` (see [`DynamicLibrary::symbol`]), so a stale 2-method `v1`
/// `.so` would dlsym/transmute fine yet hit a vtable-slot UB at call time.
/// Renaming the symbol makes dlsym fail cleanly on an old plugin instead.
pub const ENTRY_SYMBOL: &[u8] = b"tellur_timeline_collection_v2\0";

type EntryFn = unsafe extern "Rust" fn() -> Box<dyn TimelineCollection>;

/// Metadata for one timeline exposed by a plugin.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineInfo {
    pub id: String,
    pub title: String,
    pub duration: f32,
    /// A resolve-time error for this timeline, if its tree failed to resolve
    /// (e.g. a media probe failure or a timeless root). Mirrors the
    /// `last_error` display the server already surfaces for plugin load
    /// failures; `None` when the timeline resolved cleanly.
    pub error: Option<String>,
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

    /// The resolved arrangement tree for `id`, for the live UI to introspect.
    ///
    /// Defaulted to `None` so existing collections can migrate in stages
    /// (audit B.4): a collection that has not yet built a resolved tree simply
    /// returns nothing here.
    fn arrangement(&self, _id: &str) -> Option<Arrangement> {
        None
    }
}

/// Wraps a single resolved [`TimelineComponent`] as a one-entry collection.
///
/// The tree is resolved ONCE at collection construction (plugin-side — the host
/// only sees `Box<dyn TimelineCollection>`, audit B1) and the result is stored
/// as a [`Result`]: resolving probes media and can fail, but the entry fn
/// cannot return a `Result` across the dylib boundary and panicking there is
/// UB-adjacent (audit M5). So the error is stored and surfaced per query —
/// `build` yields `None`, `timelines` carries the error string, `arrangement`
/// yields `None`.
pub struct SingleTimeline {
    id: &'static str,
    title: &'static str,
    resolved: Result<ResolvedTimeline, ResolveError>,
}

/// Resolves `root` and wraps it as a one-entry [`TimelineCollection`].
///
/// `resolve` CONSUMES the tree (audit M1) and can fail; the result is kept
/// intact so the entry fn stays panic-free.
pub fn single_timeline<T: TimelineComponent + Send + 'static>(
    id: &'static str,
    title: &'static str,
    root: T,
) -> SingleTimeline {
    SingleTimeline {
        id,
        title,
        resolved: resolve(root),
    }
}

impl SingleTimeline {
    fn resolved(&self) -> Option<&ResolvedTimeline> {
        self.resolved.as_ref().ok()
    }
}

impl TimelineCollection for SingleTimeline {
    fn timelines(&self) -> Vec<TimelineInfo> {
        let (duration, error) = match &self.resolved {
            Ok(resolved) => (resolved.duration(), None),
            Err(e) => (0.0, Some(e.to_string())),
        };
        vec![TimelineInfo {
            id: self.id.to_owned(),
            title: self.title.to_owned(),
            duration,
            error,
        }]
    }

    fn build(
        &self,
        id: &str,
        t: TimelineTime,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        if id != self.id {
            return None;
        }
        self.resolved()?.frame(t, target, ctx)
    }

    fn arrangement(&self, id: &str) -> Option<Arrangement> {
        if id != self.id {
            return None;
        }
        // Root offset 0: the resolved tree's start coincides with the global
        // axis, so the walk stamps absolute starts/ends from there.
        Some(self.resolved()?.source().arrangement(0.0))
    }
}

/// Adapts an old closure-based [`Timeline`] to the new [`TimelineComponent`]
/// model so the existing demo scene (which still builds a `Timeline`) can be
/// served by the migrated collection without rewriting it.
///
/// A `Timeline` is opaque (a closure with a fixed `duration`), so this presents
/// it as a single timed leaf: its `frame` plays the timeline at the global
/// clock, its length is the timeline's `duration`, and its `arrangement` is one
/// Video-kind node spanning `[0, duration]`.
///
/// `TimelineComponent` requires `PartialEq + Hash` (via the `DynEq`/`DynHash`
/// super-traits). A `Timeline` closure is neither, and timeline nodes are never
/// memoized through `ctx.render` (`.sketch/02 §11`) — this identity is only the
/// builder-marker key — so the wrapper compares all instances equal and hashes
/// to a constant. There is exactly one per collection, so that is sound.
pub struct LegacyTimeline<T: Timeline + Send> {
    timeline: T,
}

impl<T: Timeline + Send> LegacyTimeline<T> {
    /// Wraps `timeline` so it can be placed in the new timeline world.
    pub fn new(timeline: T) -> Self {
        Self { timeline }
    }
}

impl<T: Timeline + Send> PartialEq for LegacyTimeline<T> {
    fn eq(&self, _other: &Self) -> bool {
        // Opaque closure timeline: treat the single wrapped instance as its own
        // identity. Not used as a per-frame cache key (see the type doc).
        true
    }
}

impl<T: Timeline + Send> Eq for LegacyTimeline<T> {}

impl<T: Timeline + Send> Hash for LegacyTimeline<T> {
    fn hash<H: Hasher>(&self, _state: &mut H) {
        // Constant hash: consistent with the all-equal `PartialEq` above.
    }
}

impl<T: Timeline + Send + 'static> TimelineComponent for LegacyTimeline<T> {
    fn duration(&self) -> Option<f32> {
        Some(self.timeline.duration())
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        Some(self.timeline.build(clock.global(), target, ctx))
    }

    fn arrangement(&self, offset: f32) -> Arrangement {
        // One Video-kind node spanning the whole timeline; no source crop,
        // no triggers, no children — the legacy timeline is opaque.
        Arrangement {
            kind: NodeKind::Video,
            label: String::new(),
            start: offset,
            end: offset + self.timeline.duration(),
            trim: None,
            triggers: Vec::new(),
            children: Vec::new(),
        }
    }
}

/// Exports a single [`TimelineComponent`] builder from a `cdylib`.
///
/// ```ignore
/// fn build() -> impl tellur_core::timeline_component::TimelineComponent + Send { ... }
/// tellur_live::export_timeline!("main", "Main", build);
/// ```
#[macro_export]
macro_rules! export_timeline {
    ($id:expr, $title:expr, $builder:path) => {
        #[no_mangle]
        pub extern "Rust" fn tellur_timeline_collection_v2(
        ) -> ::std::boxed::Box<dyn $crate::TimelineCollection> {
            ::std::boxed::Box::new($crate::single_timeline($id, $title, $builder()))
        }
    };
}

/// Exports a single OLD closure-based [`tellur_core::timeline::Timeline`]
/// builder from a `cdylib`, wrapping it in a [`LegacyTimeline`] adapter so it
/// serves through the migrated collection unchanged.
///
/// ```ignore
/// fn build() -> impl tellur_core::timeline::Timeline + Send { ... }
/// tellur_live::export_legacy_timeline!("main", "Main", build);
/// ```
#[macro_export]
macro_rules! export_legacy_timeline {
    ($id:expr, $title:expr, $builder:path) => {
        #[no_mangle]
        pub extern "Rust" fn tellur_timeline_collection_v2(
        ) -> ::std::boxed::Box<dyn $crate::TimelineCollection> {
            ::std::boxed::Box::new($crate::single_timeline(
                $id,
                $title,
                $crate::LegacyTimeline::new($builder()),
            ))
        }
    };
}

/// Exports a custom [`TimelineCollection`] builder from a `cdylib`.
#[macro_export]
macro_rules! export_timeline_collection {
    ($builder:path) => {
        #[no_mangle]
        pub extern "Rust" fn tellur_timeline_collection_v2(
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
    hash: u64,
}

impl SourceStamp {
    fn cache_key(self) -> String {
        format!("{:016x}-{:x}", self.hash, self.len)
    }

    fn same_content(self, other: Self) -> bool {
        self.len == other.len && self.hash == other.hash
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

    pub fn cache_key(&self) -> Option<&str> {
        self.loaded.as_ref().map(|loaded| loaded.cache_key.as_str())
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn reload_if_changed(&mut self) -> Result<bool, PluginLoadError> {
        let stamp = source_stamp(&self.source_path)?;
        let changed = self
            .loaded
            .as_ref()
            .map(|loaded| !loaded.stamp.same_content(stamp))
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
        hash: file_hash(path)?,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use tellur_core::geometry::{Constraints, Vec2};
    use tellur_core::raster::{PixelFormat, RasterComponent};
    use tellur_core::timeline_component::Timed;
    use tellur_core::timeline_container::Timeline as TimelineContainer;

    // A trivial timeless visual so a small NEW-API timeline can be built without
    // pulling in media decode. Reaches the timeline world via the one-way
    // blanket over `RasterComponent`.
    #[derive(PartialEq, Hash)]
    struct Dot;

    impl RasterComponent for Dot {
        fn layout(&self, _c: Constraints) -> Vec2 {
            Vec2(1.0, 1.0)
        }

        fn render(&self, _s: Vec2, _t: Resolution, _ctx: &mut dyn RenderContext) -> RasterImage {
            RasterImage::cpu(1, 1, PixelFormat::Rgba8, vec![0u8, 0, 0, 0])
        }
    }

    // Builds an overlay `Timeline` of two windowed visuals — a small NEW-API
    // timeline that resolves to a determinate length (no media probe).
    fn build_new_api_timeline() -> TimelineContainer {
        TimelineContainer::builder()
            .child(Dot.at(0.0..2.0))
            .child(Dot.at(0.0..3.0))
            .build()
    }

    #[test]
    fn single_timeline_surfaces_resolved_duration() {
        let collection = single_timeline("main", "Main", build_new_api_timeline());
        let infos = collection.timelines();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].id, "main");
        assert_eq!(infos[0].title, "Main");
        // The overlay length is the max child window end (3.0).
        assert_eq!(infos[0].duration, 3.0);
        assert_eq!(infos[0].error, None);
    }

    #[test]
    fn single_timeline_arrangement_walks_the_resolved_tree() {
        let collection = single_timeline("main", "Main", build_new_api_timeline());

        // A non-matching id yields nothing.
        assert!(collection.arrangement("other").is_none());

        let root = collection
            .arrangement("main")
            .expect("the resolved tree has an arrangement");

        // The root is the overlay Timeline spanning [0, 3].
        assert_eq!(root.kind, NodeKind::Timeline);
        assert_eq!(root.start, 0.0);
        assert_eq!(root.end, 3.0);
        assert!(root.trim.is_none());

        // Two children, each a Caption-kind visual leaf (via the blanket).
        assert_eq!(root.children.len(), 2);
        for child in &root.children {
            assert_eq!(child.kind, NodeKind::Caption);
            assert!(child.children.is_empty());
        }
    }

    #[test]
    fn legacy_timeline_adapter_arrangement_is_one_video_span() {
        // The legacy adapter presents an opaque closure timeline as a single
        // Video-kind node spanning its whole duration.
        let legacy = LegacyTimeline::new(tellur_core::timeline::timeline(
            4.0,
            |_t, target: Resolution, _ctx| {
                RasterImage::cpu(
                    target.width,
                    target.height,
                    PixelFormat::Rgba8,
                    vec![0u8; (target.width * target.height * 4) as usize],
                )
            },
        ));
        let collection = single_timeline("main", "Main", legacy);

        assert_eq!(collection.timelines()[0].duration, 4.0);
        let root = collection.arrangement("main").expect("resolves to a span");
        assert_eq!(root.kind, NodeKind::Video);
        assert_eq!(root.start, 0.0);
        assert_eq!(root.end, 4.0);
        assert!(root.children.is_empty());
    }
}
