use std::cmp::Reverse;
use std::collections::HashMap;
use std::error::Error;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, LazyLock, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};

use lru::LruCache;
use tellur_core::cache_budget::{cache_ram_capacity, try_reserve_cache_ram, BudgetReservation};
use tellur_core::raster::{CpuRasterImage, PixelFormat, Resolution};
use tellur_core::render_context::{GpuPreference, RenderContext};
use tellur_core::time::TimelineTime;
use tellur_core::timeline_component::{Arrangement, AudioBuffer, NodeKind};
use tellur_renderer::render_context::{CacheMetrics, TypeStats};
use tellur_renderer::{CachingRenderContext, ColorRange};

use crate::build_watch::{
    describe_build, run_build_once, start_build_watcher, AutoBuildOptions, CompileSnapshot,
    CompileState,
};
use crate::plugin::HotReloadPlugin;
use tellur_plugin::TimelineInfo;

/// Live preview re-encodes short MP4 segments repeatedly. Keeping the render
/// cache below the renderer's export-oriented 1 GiB default avoids memory
/// pressure and LRU churn making playback look stalled.
const LIVE_PREVIEW_CACHE_BYTES: usize = 256 * 1024 * 1024;
const VIDEO_SEGMENT_CACHE_BYTES: usize = 512 * 1024 * 1024;
const VIDEO_SEGMENT_CACHE_ENTRIES: usize = 128;

static VIDEO_SEGMENT_CACHE: LazyLock<Mutex<VideoSegmentCache>> =
    LazyLock::new(|| Mutex::new(VideoSegmentCache::default()));

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct VideoSegmentCacheKey {
    plugin_cache_key: String,
    timeline_id: String,
    start_seconds_bits: u32,
    video_seconds_bits: u32,
    width: u32,
    height: u32,
    fps: u32,
    gop: u32,
    crf: u8,
    motion_blur: bool,
    color_range: ColorRange,
}

struct VideoSegmentCache {
    entries: LruCache<VideoSegmentCacheKey, CachedVideoSegment>,
    bytes: usize,
}

struct CachedVideoSegment {
    body: Arc<Vec<u8>>,
    _reservation: BudgetReservation,
}

impl Default for VideoSegmentCache {
    fn default() -> Self {
        Self {
            entries: LruCache::unbounded(),
            bytes: 0,
        }
    }
}

impl VideoSegmentCache {
    fn get(&mut self, key: &VideoSegmentCacheKey) -> Option<Arc<Vec<u8>>> {
        self.entries.get(key).map(|entry| Arc::clone(&entry.body))
    }

    fn insert(&mut self, key: VideoSegmentCacheKey, body: Vec<u8>) {
        let bytes = body.len();
        let capacity = cache_ram_capacity(VIDEO_SEGMENT_CACHE_BYTES);
        if bytes > capacity {
            return;
        }
        while self.bytes + bytes > capacity || self.entries.len() >= VIDEO_SEGMENT_CACHE_ENTRIES {
            let Some((_, old)) = self.entries.pop_lru() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(old.body.len());
        }
        let Some(reservation) = try_reserve_cache_ram(bytes) else {
            return;
        };
        let entry = CachedVideoSegment {
            body: Arc::new(body),
            _reservation: reservation,
        };
        if let Some(old) = self.entries.put(key, entry) {
            self.bytes = self.bytes.saturating_sub(old.body.len());
        }
        self.bytes += bytes;
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}

fn cached_video_segment(key: &VideoSegmentCacheKey) -> Option<Arc<Vec<u8>>> {
    VIDEO_SEGMENT_CACHE.lock().ok()?.get(key)
}

fn cache_video_segment(key: VideoSegmentCacheKey, body: Vec<u8>) {
    if let Ok(mut cache) = VIDEO_SEGMENT_CACHE.lock() {
        cache.insert(key, body);
    }
}

fn clear_video_segment_cache() {
    if let Ok(mut cache) = VIDEO_SEGMENT_CACHE.lock() {
        cache.clear();
    }
}

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub plugin_path: PathBuf,
    pub project_name: String,
    pub bind: String,
    pub resolution: Resolution,
    pub fps: u32,
    pub color_range: ColorRange,
    pub gpu_preference: GpuPreference,
    pub verbose: bool,
    pub auto_build: Option<AutoBuildOptions>,
}

pub fn serve(options: ServerOptions) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind(&options.bind)?;
    let local_addr = listener.local_addr()?;
    eprintln!("tellur live listening on http://{local_addr}");
    eprintln!("plugin: {}", options.plugin_path.display());
    if let Some(auto_build) = &options.auto_build {
        eprintln!("auto build: {}", describe_build(auto_build));
        eprintln!("running initial build");
        run_build_once(auto_build).map_err(|e| -> Box<dyn Error> { e.into() })?;
    }

    let prewarm_gpu = options.gpu_preference.prefers_gpu();
    let compile_state = options
        .auto_build
        .clone()
        .map(start_build_watcher)
        .unwrap_or_else(CompileState::compiled);

    let app = Arc::new(Mutex::new(PreviewApp {
        plugin: HotReloadPlugin::new(options.plugin_path),
        project_name: options.project_name,
        ctx: CachingRenderContext::with_capacity_bytes(LIVE_PREVIEW_CACHE_BYTES)
            .with_volatile_large_admission()
            .with_gpu_preference(options.gpu_preference),
        resolution: options.resolution,
        fps: options.fps,
        color_range: options.color_range,
        verbose: options.verbose,
        compile_state,
    }));
    {
        let mut app = app
            .lock()
            .map_err(|_| -> Box<dyn Error> { "preview app lock poisoned".into() })?;
        app.reload_plugin_if_changed()?;
    }
    if prewarm_gpu {
        start_preview_prewarm(Arc::clone(&app));
    }

    let video_epochs: Arc<Mutex<HashMap<String, Arc<AtomicU64>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let app = Arc::clone(&app);
                let video_epochs = Arc::clone(&video_epochs);
                thread::spawn(move || {
                    if let Err(e) = handle_connection(app, video_epochs, stream) {
                        if !is_client_disconnect(e.as_ref()) {
                            eprintln!("request failed: {e}");
                        }
                    }
                });
            }
            Err(e) => eprintln!("accept failed: {e}"),
        }
    }
    Ok(())
}

fn start_preview_prewarm(app: Arc<Mutex<PreviewApp>>) {
    thread::spawn(move || {
        let prewarm_start = Instant::now();
        match preview_prewarm(&app) {
            Ok(Some((timeline_id, audio_time, render_time, build_time, readback_time, true))) => {
                println!(
                    "preview-prewarm timeline={} audio={:.2}ms render={:.2}ms build={:.2}ms readback={:.2}ms total={:.2}ms",
                    timeline_id,
                    ms(audio_time),
                    ms(render_time),
                    ms(build_time),
                    ms(readback_time),
                    ms(prewarm_start.elapsed()),
                );
            }
            Ok(_) => {}
            Err(e) => eprintln!("preview prewarm failed: {e}"),
        }
    });
}

type PreviewPrewarmStats = (String, Duration, Duration, Duration, Duration, bool);

fn preview_prewarm(
    app: &Arc<Mutex<PreviewApp>>,
) -> Result<Option<PreviewPrewarmStats>, Box<dyn Error>> {
    let mut app = app
        .lock()
        .map_err(|_| -> Box<dyn Error> { "preview app lock poisoned".into() })?;
    app.reload_plugin_if_changed()?;
    let Some(info) = app
        .plugin
        .collection()?
        .timelines()
        .into_iter()
        .find(|info| info.error.is_none() && info.duration > 0.0)
    else {
        return Ok(None);
    };
    let verbose = app.verbose;
    let resolution = app.resolution;
    let audio_start = Instant::now();
    let _ = app.plugin.collection()?.render_audio_window(
        &info.id,
        0.0,
        info.duration.min(1.0),
        AUDIO_RATE,
        AUDIO_CHANNELS,
    );
    let audio_time = audio_start.elapsed();
    let frame = app.render_video_rgba(&info.id, 0.0, resolution, false, false)?;
    Ok(Some((
        info.id,
        audio_time,
        frame.render_time,
        frame.build_time,
        frame.readback_time,
        verbose,
    )))
}

fn handle_connection(
    app: Arc<Mutex<PreviewApp>>,
    video_epochs: Arc<Mutex<HashMap<String, Arc<AtomicU64>>>>,
    mut stream: TcpStream,
) -> Result<(), Box<dyn Error>> {
    let request = match read_request(&mut stream)? {
        Some(request) => request,
        None => return Ok(()),
    };

    if request.method != "GET" {
        return write_response(
            &mut stream,
            405,
            "Method Not Allowed",
            "text/plain; charset=utf-8",
            b"method not allowed",
        );
    }

    let path = request.path.clone();
    match path.as_str() {
        "/api/video.mp4" | "/api/video" => {
            handle_video_stream(app, video_epochs, stream, request.query)
        }
        "/api/events" => handle_event_stream(app, stream),
        "/api/info" | "/api/frame" | "/api/stream" | "/api/arrangement" => {
            let mut app = app
                .lock()
                .map_err(|_| -> Box<dyn Error> { "preview app lock poisoned".into() })?;
            app.handle_api(stream, request)
        }
        other => serve_static(&mut stream, other),
    }
}

fn handle_event_stream(
    app: Arc<Mutex<PreviewApp>>,
    mut stream: TcpStream,
) -> Result<(), Box<dyn Error>> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream; charset=utf-8\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n"
    )?;

    let mut last_body = String::new();
    loop {
        let body = {
            let mut app = app
                .lock()
                .map_err(|_| -> Box<dyn Error> { "preview app lock poisoned".into() })?;
            app.info_body()?
        };
        if body != last_body {
            write!(stream, "event: info\ndata: {body}\n\n")?;
            stream.flush()?;
            last_body = body;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn serve_static(stream: &mut TcpStream, path: &str) -> Result<(), Box<dyn Error>> {
    let asset = match path {
        "/" | "/index.html" => Some(StaticAsset {
            body: WEB_INDEX_HTML,
            mime: "text/html; charset=utf-8",
        }),
        "/assets/index.js" => Some(StaticAsset {
            body: WEB_INDEX_JS,
            mime: "application/javascript; charset=utf-8",
        }),
        "/assets/index.css" => Some(StaticAsset {
            body: WEB_INDEX_CSS,
            mime: "text/css; charset=utf-8",
        }),
        _ => None,
    };
    match asset {
        Some(asset) => write_response(stream, 200, "OK", asset.mime, asset.body),
        None => write_response(
            stream,
            404,
            "Not Found",
            "text/plain; charset=utf-8",
            b"not found",
        ),
    }
}

struct StaticAsset {
    body: &'static [u8],
    mime: &'static str,
}

const WEB_INDEX_HTML: &[u8] = include_bytes!("../web/dist/index.html");
const WEB_INDEX_JS: &[u8] = include_bytes!("../web/dist/assets/index.js");
const WEB_INDEX_CSS: &[u8] = include_bytes!("../web/dist/assets/index.css");

fn is_client_disconnect(error: &(dyn Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            return matches!(
                io.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
            );
        }
        current = error.source();
    }
    false
}

struct PreviewApp {
    plugin: HotReloadPlugin,
    project_name: String,
    ctx: CachingRenderContext,
    resolution: Resolution,
    fps: u32,
    color_range: ColorRange,
    verbose: bool,
    compile_state: CompileState,
}

impl PreviewApp {
    fn reload_plugin_if_changed(&mut self) -> Result<bool, Box<dyn Error>> {
        let changed = self.plugin.reload_if_changed()?;
        if changed {
            self.ctx.clear();
            self.ctx.clear_metrics();
            clear_video_segment_cache();
        }
        Ok(changed)
    }

    fn is_media_cacheable(&self, query: &HashMap<String, String>) -> bool {
        matches!(
            (query.get("v").map(String::as_str), self.plugin.cache_key()),
            (Some(requested), Some(current)) if requested == current
        )
    }

    fn media_cache_control(&self, query: &HashMap<String, String>) -> &'static str {
        if self.is_media_cacheable(query) {
            "public, max-age=31536000, immutable"
        } else {
            "no-store"
        }
    }

    fn handle_api(&mut self, stream: TcpStream, request: Request) -> Result<(), Box<dyn Error>> {
        match request.path.as_str() {
            "/api/info" => self.handle_info(stream),
            "/api/frame" => self.handle_frame(stream, &request.query),
            "/api/stream" => self.handle_stream(stream, &request.query),
            "/api/arrangement" => self.handle_arrangement(stream, &request.query),
            _ => unreachable!("non-api routes are handled before acquiring the preview lock"),
        }
    }

    fn handle_info(&mut self, mut stream: TcpStream) -> Result<(), Box<dyn Error>> {
        let body = self.info_body()?;
        write_response(
            &mut stream,
            200,
            "OK",
            "application/json; charset=utf-8",
            body.as_bytes(),
        )
    }

    fn info_body(&mut self) -> Result<String, Box<dyn Error>> {
        self.reload_plugin_if_changed()?;
        let timelines = self.plugin.collection()?.timelines();
        let compile = self.compile_state.snapshot();
        Ok(info_json(
            &self.project_name,
            self.resolution,
            self.fps,
            &timelines,
            self.plugin.last_error(),
            self.plugin.cache_key().unwrap_or(""),
            &compile,
        ))
    }

    fn handle_arrangement(
        &mut self,
        mut stream: TcpStream,
        query: &HashMap<String, String>,
    ) -> Result<(), Box<dyn Error>> {
        self.reload_plugin_if_changed()?;
        let collection = self.plugin.collection()?;
        let timelines = collection.timelines();
        let Some(info) = select_timeline(&timelines, query.get("timeline")) else {
            return write_response(
                &mut stream,
                404,
                "Not Found",
                "application/json; charset=utf-8",
                b"null",
            );
        };
        let body = match collection.arrangement(&info.id) {
            Some(arrangement) => arrangement_json(&arrangement),
            // The collection has not resolved a tree for this id (a failed
            // resolve, or a not-yet-migrated collection): emit `null` so the UI
            // can fall back to its flat view.
            None => "null".to_owned(),
        };
        write_response(
            &mut stream,
            200,
            "OK",
            "application/json; charset=utf-8",
            body.as_bytes(),
        )
    }

    fn handle_frame(
        &mut self,
        mut stream: TcpStream,
        query: &HashMap<String, String>,
    ) -> Result<(), Box<dyn Error>> {
        match FrameFormat::from_query(query) {
            FrameFormat::Png => {
                let rendered = self.render_png(query)?;
                if self.verbose {
                    log_frame_stats(&rendered.stats);
                }
                let headers = rendered.stats.headers();
                write_response_with_headers_and_cache_control(
                    &mut stream,
                    200,
                    "OK",
                    "image/png",
                    &headers,
                    &rendered.body,
                    self.media_cache_control(query),
                )
            }
            FrameFormat::Rgba => {
                let rendered = self.render_rgba(query)?;
                if self.verbose {
                    log_frame_stats(&rendered.stats);
                }
                let headers = rendered.stats.headers();
                write_response_with_headers_and_cache_control(
                    &mut stream,
                    200,
                    "OK",
                    "application/vnd.tellur.rgba",
                    &headers,
                    &rendered.body,
                    self.media_cache_control(query),
                )
            }
        }
    }

    fn handle_stream(
        &mut self,
        stream: TcpStream,
        query: &HashMap<String, String>,
    ) -> Result<(), Box<dyn Error>> {
        match FrameFormat::from_query(query) {
            FrameFormat::Png => self.handle_png_stream(stream, query),
            FrameFormat::Rgba => self.handle_rgba_stream(stream, query),
        }
    }

    fn handle_png_stream(
        &mut self,
        mut stream: TcpStream,
        query: &HashMap<String, String>,
    ) -> Result<(), Box<dyn Error>> {
        let fps = request_fps(query, self.fps.max(1));
        let resolution = request_resolution(query, self.resolution);
        let timeline_id = query.get("timeline").cloned();
        let mut seconds = query
            .get("time")
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.0);

        write!(
            stream,
            "HTTP/1.1 200 OK\r\n\
             Content-Type: multipart/x-mixed-replace; boundary=tellur-frame\r\n\
             Cache-Control: no-store\r\n\
             Connection: close\r\n\r\n"
        )?;

        let frame_step = 1.0 / fps as f32;
        let frame_duration = Duration::from_secs_f32(frame_step);
        loop {
            let frame_start = Instant::now();
            let mut q = HashMap::new();
            q.insert("time".to_owned(), seconds.to_string());
            q.insert("width".to_owned(), resolution.width.to_string());
            q.insert("height".to_owned(), resolution.height.to_string());
            if let Some(motion_blur) = query.get("motion_blur") {
                q.insert("motion_blur".to_owned(), motion_blur.clone());
            }
            if let Some(id) = &timeline_id {
                q.insert("timeline".to_owned(), id.clone());
            }
            let rendered = self.render_png(&q)?;
            if self.verbose {
                log_frame_stats(&rendered.stats);
            }
            write!(
                stream,
                "--tellur-frame\r\n\
                 Content-Type: image/png\r\n\
                 Content-Length: {}\r\n\r\n",
                rendered.body.len()
            )?;
            stream.write_all(&rendered.body)?;
            stream.write_all(b"\r\n")?;
            stream.flush()?;
            seconds += frame_step;
            sleep_remainder(frame_duration, frame_start.elapsed());
        }
    }

    fn handle_rgba_stream(
        &mut self,
        mut stream: TcpStream,
        query: &HashMap<String, String>,
    ) -> Result<(), Box<dyn Error>> {
        let fps = request_fps(query, self.fps.max(1));
        let resolution = request_resolution(query, self.resolution);
        let timeline_id = query.get("timeline").cloned();
        let mut seconds = query
            .get("time")
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.0);
        let frame_bytes = (resolution.width as usize) * (resolution.height as usize) * 4;

        write!(
            stream,
            "HTTP/1.1 200 OK\r\n\
             Content-Type: application/vnd.tellur.rgba-stream\r\n\
             X-Tellur-Width: {}\r\n\
             X-Tellur-Height: {}\r\n\
             X-Tellur-Fps: {}\r\n\
             X-Tellur-Frame-Bytes: {}\r\n\
             Cache-Control: no-store\r\n\
             Connection: close\r\n\r\n",
            resolution.width, resolution.height, fps, frame_bytes,
        )?;

        let frame_step = 1.0 / fps as f32;
        let frame_duration = Duration::from_secs_f32(frame_step);
        loop {
            let frame_start = Instant::now();
            let mut q = HashMap::new();
            q.insert("time".to_owned(), seconds.to_string());
            q.insert("format".to_owned(), "rgba".to_owned());
            q.insert("width".to_owned(), resolution.width.to_string());
            q.insert("height".to_owned(), resolution.height.to_string());
            if let Some(motion_blur) = query.get("motion_blur") {
                q.insert("motion_blur".to_owned(), motion_blur.clone());
            }
            if let Some(id) = &timeline_id {
                q.insert("timeline".to_owned(), id.clone());
            }
            let rendered = self.render_rgba(&q)?;
            if self.verbose {
                log_frame_stats(&rendered.stats);
            }
            stream.write_all(&rendered.body)?;
            stream.flush()?;
            seconds += frame_step;
            sleep_remainder(frame_duration, frame_start.elapsed());
        }
    }

    fn render_video_rgba(
        &mut self,
        timeline_id: &str,
        seconds: f32,
        resolution: Resolution,
        motion_blur: bool,
        collect_stats: bool,
    ) -> Result<VideoFrame, Box<dyn Error>> {
        self.ctx.set_motion_blur_enabled(motion_blur);
        let before = collect_stats.then(|| self.ctx.metrics());
        let render_start = Instant::now();
        let build_start = Instant::now();
        let image = self
            .plugin
            .collection()?
            .build(
                timeline_id,
                TimelineTime::new(seconds),
                resolution,
                &mut self.ctx,
            )
            .ok_or("timeline did not produce a frame")?;
        let build_time = build_start.elapsed();
        let readback_start = Instant::now();
        let image = self.ctx.readback(image);
        let readback_time = readback_start.elapsed();
        let render_time = render_start.elapsed();
        if image.format != PixelFormat::Rgba8 {
            return Err(format!("h264 stream requires Rgba8, got {:?}", image.format).into());
        }
        let stats = before.map(|before| {
            let after = self.ctx.metrics();
            let gpu_init_error = self.ctx.gpu_init_error().map(str::to_owned);
            (
                after.hits.saturating_sub(before.hits),
                after.misses.saturating_sub(before.misses),
                after.bytes_cached,
                after.gpu_available,
                after.gpu_init_attempted,
                gpu_init_error,
                format!("{:?}", after.gpu_preference),
                after.gpu.total_ops().saturating_sub(before.gpu.total_ops()),
                after.gpu.readbacks.saturating_sub(before.gpu.readbacks),
            )
        });
        let (
            cache_hits,
            cache_misses,
            bytes_cached,
            gpu_available,
            gpu_init_attempted,
            gpu_init_error,
            gpu_preference,
            gpu_ops,
            gpu_readbacks,
        ) = stats.unwrap_or_else(|| (0, 0, 0, false, false, None, String::new(), 0, 0));

        Ok(VideoFrame {
            image,
            render_time,
            build_time,
            readback_time,
            cache_hits,
            cache_misses,
            bytes_cached,
            gpu_available,
            gpu_init_attempted,
            gpu_init_error,
            gpu_preference,
            gpu_ops,
            gpu_readbacks,
        })
    }

    fn render_png(
        &mut self,
        query: &HashMap<String, String>,
    ) -> Result<RenderedFrame, Box<dyn Error>> {
        let mut rendered = self.render_image(query)?;
        if request_video_color(query) {
            rendered.image = video_color_preview_image(
                &rendered.image,
                request_color_range(query, self.color_range),
            )?;
        }

        let encode_start = Instant::now();
        let mut body = Vec::new();
        export_preview_png(&rendered.image, &mut body)?;
        let encode_time = encode_start.elapsed();

        let mut stats = rendered.stats;
        stats.output_format = FrameFormat::Png;
        stats.encode_time = encode_time;
        stats.total_time = rendered.total_start.elapsed();
        stats.output_bytes = body.len();

        Ok(RenderedFrame { body, stats })
    }

    fn render_rgba(
        &mut self,
        query: &HashMap<String, String>,
    ) -> Result<RenderedFrame, Box<dyn Error>> {
        let rendered = self.render_image(query)?;
        if rendered.image.format != PixelFormat::Rgba8 {
            return Err(format!(
                "raw rgba output requires Rgba8, got {:?}",
                rendered.image.format
            )
            .into());
        }

        let encode_start = Instant::now();
        let body = rendered.image.pixels.as_ref().to_vec();
        let encode_time = encode_start.elapsed();
        let mut stats = rendered.stats;
        stats.output_format = FrameFormat::Rgba;
        stats.encode_time = encode_time;
        stats.total_time = rendered.total_start.elapsed();
        stats.output_bytes = body.len();

        Ok(RenderedFrame { body, stats })
    }

    fn render_image(
        &mut self,
        query: &HashMap<String, String>,
    ) -> Result<RenderedImage, Box<dyn Error>> {
        let total_start = Instant::now();
        self.reload_plugin_if_changed()?;
        let timelines = self.plugin.collection()?.timelines();
        let Some(info) = select_timeline(&timelines, query.get("timeline")) else {
            return Err("timeline not found".into());
        };
        let fps = request_fps(query, self.fps.max(1));
        let seconds = query
            .get("frame")
            .and_then(|v| v.parse::<u64>().ok())
            .map(|frame| frame as f32 / fps as f32)
            .or_else(|| query.get("time").and_then(|v| v.parse::<f32>().ok()))
            .unwrap_or(0.0);
        // Clamp into the half-open renderable range so a `time=<duration>`
        // request (a frontend scrubbed to the very end) returns the last frame
        // instead of erroring with `timeline did not produce a frame`.
        let seconds = clamp_to_renderable(seconds, info.duration, fps);
        let resolution = request_resolution(query, self.resolution);
        self.ctx.set_motion_blur_enabled(request_motion_blur(query));

        let before = self.ctx.metrics();
        let render_start = Instant::now();
        let image = self
            .plugin
            .collection()?
            .build(
                &info.id,
                TimelineTime::new(seconds),
                resolution,
                &mut self.ctx,
            )
            .ok_or("timeline did not produce a frame")?;
        let image = self.ctx.readback(image);
        let render_time = render_start.elapsed();
        let after = self.ctx.metrics();
        let gpu_init_error = self.ctx.gpu_init_error().map(str::to_owned);

        Ok(RenderedImage {
            image,
            stats: FrameRenderStats {
                timeline_id: info.id.clone(),
                seconds,
                resolution,
                render_time,
                encode_time: Duration::ZERO,
                total_time: render_time,
                output_format: FrameFormat::Rgba,
                output_bytes: 0,
                cache_hits: after.hits.saturating_sub(before.hits),
                cache_misses: after.misses.saturating_sub(before.misses),
                bytes_cached: after.bytes_cached,
                gpu_available: after.gpu_available,
                gpu_init_attempted: after.gpu_init_attempted,
                gpu_init_error,
                gpu_preference: format!("{:?}", after.gpu_preference),
                gpu_ops: after.gpu.total_ops().saturating_sub(before.gpu.total_ops()),
                gpu_readbacks: after.gpu.readbacks.saturating_sub(before.gpu.readbacks),
            },
            total_start,
        })
    }
}

struct VideoStreamSetup {
    timeline_id: String,
    duration: f32,
    /// The full timeline length, used to clamp each frame's requested time into
    /// the half-open renderable range (`< total_duration`).
    total_duration: f32,
    fps: u32,
    resolution: Resolution,
    gop: u32,
    crf: u8,
    motion_blur: bool,
    color_range: ColorRange,
    start_seconds: f32,
    cache_control: &'static str,
    realtime: bool,
    verbose: bool,
}

fn video_segment_cache_key(
    setup: &VideoStreamSetup,
    plugin_cache_key: &str,
    video_seconds: f32,
) -> VideoSegmentCacheKey {
    VideoSegmentCacheKey {
        plugin_cache_key: plugin_cache_key.to_owned(),
        timeline_id: setup.timeline_id.clone(),
        start_seconds_bits: setup.start_seconds.to_bits(),
        video_seconds_bits: video_seconds.to_bits(),
        width: setup.resolution.width,
        height: setup.resolution.height,
        fps: setup.fps,
        gop: setup.gop,
        crf: setup.crf,
        motion_blur: setup.motion_blur,
        color_range: setup.color_range,
    }
}

fn write_video_stream_headers(
    stream: &mut TcpStream,
    setup: &VideoStreamSetup,
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\n\
         Content-Type: video/mp4\r\n\
         X-Tellur-Width: {}\r\n\
         X-Tellur-Height: {}\r\n\
         X-Tellur-Fps: {}\r\n\
         X-Tellur-Gop: {}\r\n\
         X-Tellur-Color-Range: {}\r\n\
         Cache-Control: {}\r\n\
         Connection: close\r\n\r\n",
        setup.resolution.width,
        setup.resolution.height,
        setup.fps,
        setup.gop,
        setup.color_range.as_str(),
        setup.cache_control,
    )
}

struct VideoFrame {
    image: CpuRasterImage,
    render_time: Duration,
    build_time: Duration,
    readback_time: Duration,
    cache_hits: u64,
    cache_misses: u64,
    bytes_cached: usize,
    gpu_available: bool,
    gpu_init_attempted: bool,
    gpu_init_error: Option<String>,
    gpu_preference: String,
    gpu_ops: u64,
    gpu_readbacks: u64,
}

/// The fixed audio layout the preview mux uses (matches the encoder boundary).
const AUDIO_RATE: u32 = 48_000;
const AUDIO_CHANNELS: u16 = 2;

static AUDIO_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A temp file removed on drop, so a stream's staged audio WAV is cleaned up
/// whenever `handle_video_stream` returns (including early `?` returns).
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Writes `buf` (interleaved f32) as a 32-bit IEEE float WAV to a unique temp
/// path, so ffmpeg can take it as a second input and mux it into the preview
/// stream.
fn write_temp_wav(buf: &AudioBuffer) -> std::io::Result<PathBuf> {
    let seq = AUDIO_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "tellur_live_audio_{}_{}.wav",
        std::process::id(),
        seq
    ));

    let channels = buf.channels.max(1);
    let rate = buf.rate.max(1);
    let bits: u16 = 32;
    let bytes_per_sample = (bits as u32 / 8) as usize;
    let byte_rate = rate * channels as u32 * bytes_per_sample as u32;
    let block_align = channels * bytes_per_sample as u16;
    let data_bytes = (buf.samples.len() * bytes_per_sample) as u32;

    let mut bytes = Vec::with_capacity(44 + buf.samples.len() * bytes_per_sample);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    bytes.extend_from_slice(&3u16.to_le_bytes()); // audio format = IEEE float
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for &s in &buf.samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(&path, &bytes)?;
    Ok(path)
}

fn handle_video_stream(
    app: Arc<Mutex<PreviewApp>>,
    video_epochs: Arc<Mutex<HashMap<String, Arc<AtomicU64>>>>,
    mut stream: TcpStream,
    query: HashMap<String, String>,
) -> Result<(), Box<dyn Error>> {
    let stream_start = Instant::now();
    let video_epoch = {
        let session = query
            .get("session")
            .cloned()
            .unwrap_or_else(|| "default".to_owned());
        let mut epochs = video_epochs
            .lock()
            .map_err(|_| -> Box<dyn Error> { "video epoch lock poisoned".into() })?;
        Arc::clone(
            epochs
                .entry(session)
                .or_insert_with(|| Arc::new(AtomicU64::new(0))),
        )
    };
    let stream_epoch = video_epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
    let setup_start = Instant::now();
    let setup = {
        let mut app = app
            .lock()
            .map_err(|_| -> Box<dyn Error> { "preview app lock poisoned".into() })?;
        app.reload_plugin_if_changed()?;
        let timelines = app.plugin.collection()?.timelines();
        let Some(info) = select_timeline(&timelines, query.get("timeline")) else {
            return Err("timeline not found".into());
        };

        let fps = request_fps(&query, app.fps.max(1));
        let resolution = request_resolution(&query, app.resolution);
        let gop = query
            .get("gop")
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|gop| *gop > 0)
            .unwrap_or((fps / 4).max(1));
        let crf = query
            .get("crf")
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(23);
        let start_seconds = query
            .get("time")
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.0)
            .clamp(0.0, info.duration.max(0.0));
        let remaining = (info.duration - start_seconds).max(0.0);
        let duration = query
            .get("duration")
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .map(|v| v.min(remaining))
            .unwrap_or(remaining);

        let cacheable = app.is_media_cacheable(&query);
        VideoStreamSetup {
            timeline_id: info.id.clone(),
            duration,
            total_duration: info.duration.max(0.0),
            fps,
            resolution,
            gop,
            crf,
            motion_blur: request_motion_blur(&query),
            color_range: request_color_range(&query, app.color_range),
            start_seconds,
            cache_control: if cacheable {
                "public, max-age=31536000, immutable"
            } else {
                "no-store"
            },
            realtime: !cacheable,
            verbose: app.verbose,
        }
    };
    let setup_time = setup_start.elapsed();
    if video_epoch.load(Ordering::Acquire) != stream_epoch {
        return Ok(());
    }

    // The frame-quantized video length, used to BOUND the output below with `-t`
    // (instead of `-shortest`; see that arg). This is also the loop's frame count.
    let total_frames = (setup.duration * setup.fps as f32).ceil().max(0.0) as u64;
    let video_seconds = total_frames as f32 / setup.fps as f32;
    let segment_cache_key = if setup.cache_control != "no-store" {
        query
            .get("v")
            .map(|cache_key| video_segment_cache_key(&setup, cache_key, video_seconds))
    } else {
        None
    };
    if let Some(key) = &segment_cache_key {
        if let Some(body) = cached_video_segment(key) {
            write_video_stream_headers(&mut stream, &setup)?;
            stream.write_all(&body)?;
            stream.flush()?;
            if setup.verbose {
                println!(
                    "video-stream-cache timeline={} start={:.3}s duration={:.3}s bytes={} total={:.2}ms",
                    setup.timeline_id,
                    setup.start_seconds,
                    video_seconds,
                    body.len(),
                    ms(stream_start.elapsed()),
                );
            }
            return Ok(());
        }
    }

    // Render only this stream's audio window and stage it as a temp WAV. Full
    // timeline audio can be huge, and live preview requests many short cache
    // segments, so every segment must mix only `[start, start + video_seconds)`.
    // `None` only for legacy/custom collections that do not expose audio, where
    // the stream falls back to a generated silent track. The guard removes the
    // file when this function returns.
    let audio_start = Instant::now();
    let audio_wav = {
        let mut app = app
            .lock()
            .map_err(|_| -> Box<dyn Error> { "preview app lock poisoned".into() })?;
        app.reload_plugin_if_changed()?;
        app.plugin
            .collection()
            .ok()
            .and_then(|c| {
                c.render_audio_window(
                    &setup.timeline_id,
                    setup.start_seconds,
                    video_seconds,
                    AUDIO_RATE,
                    AUDIO_CHANNELS,
                )
            })
            .and_then(|buf| write_temp_wav(&buf).ok())
            .map(TempFile)
    };
    let audio_time = audio_start.elapsed();
    let audio_source = if audio_wav.is_some() {
        "window_wav"
    } else {
        "anullsrc"
    };
    if video_epoch.load(Ordering::Acquire) != stream_epoch {
        return Ok(());
    }

    write_video_stream_headers(&mut stream, &setup)?;

    // FLAC block size aligned to ONE video frame's worth of audio samples, applied
    // to the encoder below (only when the sample rate divides evenly by fps — the
    // common 24/25/30/48/50/60 case). `-t` quantizes every segment to an integer
    // number of video frames, so an aligned block size makes each segment an exact
    // integer number of FLAC blocks: no partial trailing block, so the muxer emits
    // NO per-segment trim (`discard_padding`) at the cut. Adjacent cached segments
    // then concatenate gaplessly in MSE. With the default (~1152-sample) block the
    // final block is partial and gets trimmed mid-block, and the browser — which
    // decodes each cached segment separately and stitches at the buffer level —
    // produces an audible click at every cache-segment boundary.
    let audio_frame_size = AUDIO_RATE
        .is_multiple_of(setup.fps)
        .then(|| AUDIO_RATE / setup.fps);

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner")
        .args(["-loglevel", "error"])
        // Input 0: the raw video frames, fed live over stdin.
        .args(["-f", "rawvideo"])
        .args(["-pix_fmt", "rgba"])
        .args([
            "-s",
            &format!("{}x{}", setup.resolution.width, setup.resolution.height),
        ])
        .args(["-r", &setup.fps.to_string()])
        .args(["-i", "-"]);
    // Input 1: the audio track. A rendered WAV already starts at the stream's
    // timeline time; otherwise a generated silent track keeps every stream in
    // the same A/V structure the client's SourceBuffer expects.
    match &audio_wav {
        Some(wav) => {
            cmd.arg("-i").arg(&wav.0);
        }
        None => {
            cmd.args(["-f", "lavfi"]).arg("-i").arg(format!(
                "anullsrc=channel_layout=stereo:sample_rate={AUDIO_RATE}"
            ));
        }
    }
    let range = setup.color_range.ffmpeg_token();
    let color_vf = format!(
        "scale=out_range={range}:out_color_matrix=bt709,format=yuv420p,\
         setparams=range={range}:color_primaries=bt709:colorspace=bt709:color_trc=bt709"
    );

    cmd.args(["-c:v", "libx264"])
        .args(["-preset", "ultrafast"])
        .args(["-tune", "zerolatency"])
        // Convert the rendered full-range RGB to BT.709 YUV and
        // TAG the stream with that exact matrix/range. Without this, swscale
        // converts with its BT.601/limited defaults and writes NO color metadata,
        // so the browser falls back to its own guess (BT.709 for 720p+) and
        // decodes with a different matrix than the encode used — shifting the
        // colors versus the paused PNG (which shows the raw full-range RGB with no
        // conversion). `scale` does the conversion, `setparams` stamps all four
        // color fields onto the frames (so primaries/transfer are written too,
        // without codec-specific params), and the `-color_*` options mirror the
        // same range at stream level. Full range is the default because paused
        // PNG/RGBA frames are full-range too; use `color_range=limited` only when
        // a downstream target requires TV range.
        .args(["-vf", &color_vf])
        .args(["-pix_fmt", "yuv420p"])
        .args(["-color_primaries", "bt709"])
        .args(["-color_trc", "bt709"])
        .args(["-colorspace", "bt709"])
        .args(["-color_range", range])
        .args(["-g", &setup.gop.to_string()])
        .args(["-keyint_min", &setup.gop.to_string()])
        .args(["-sc_threshold", "0"])
        .args(["-bf", "0"])
        .args(["-refs", "1"])
        .args(["-flags", "low_delay"])
        .args(["-crf", &setup.crf.to_string()])
        // Audio FLAC, NOT AAC; map exactly one video + one audio stream. Each
        // cache segment is a SEPARATE ffmpeg encode, and AAC adds ~2048 samples of
        // encoder delay (priming) at the start of every encode plus frame-grid
        // padding at the end. Concatenating adjacent segments in MSE then leaves a
        // brief silent gap / click at each cache boundary. FLAC is lossless with
        // ZERO encoder delay and stores the exact sample count, so a segment's
        // first sample lands exactly on its start time and the boundaries are
        // gapless. `-compression_level 0` keeps the encode fast for the realtime
        // path (it stays lossless regardless of level).
        .args(["-c:a", "flac"])
        .args(["-compression_level", "0"]);
    // Align FLAC blocks to the video-frame sample grid so cache-segment seams stay
    // gapless (see `audio_frame_size`). Stream-specified to `:a` so it can't be
    // misread as a (meaningless) video option.
    if let Some(frame_size) = audio_frame_size {
        cmd.args(["-frame_size:a", &frame_size.to_string()]);
    }
    cmd.args(["-map", "0:v:0"])
        .args(["-map", "1:a:0"])
        // Bound the output to the video's frame-quantized length with `-t`, NOT
        // `-shortest`. Video frames are produced live and paced to real time, but
        // the audio WAV is a complete file ffmpeg reads instantly; `-shortest`
        // then races the fully-available audio against the still-arriving video
        // and ends the output EARLY — dropping the tail frame(s) and opening a
        // startup gap (the offline export avoids this by pre-fitting audio to the
        // video length). `-t` cuts on output PTS, so every fed frame survives.
        .args(["-t", &format!("{video_seconds:.6}")])
        .args(["-muxdelay", "0"])
        .args(["-muxpreload", "0"])
        .args(["-flush_packets", "1"])
        .args(["-f", "mp4"])
        // Fragment per GOP (`frag_keyframe`), NOT per video frame
        // (`frag_every_frame`). `frag_every_frame` cuts a fragment every video
        // frame (33.3 ms @ 30fps), but an AAC frame is 1024 samples (≈21.3 ms) /
        // 2048 (≈42.7 ms) and does not align to the video grid — so the audio
        // fragments end up with DUPLICATE / non-monotonic `tfdt`
        // (baseMediaDecodeTime), which MSE chokes on: the audio track stalls and,
        // since playback needs both tracks, the video freezes a frame or two in.
        // GOP fragments hold whole audio frames with strictly increasing `tfdt`.
        .args(["-movflags", "frag_keyframe+empty_moov+default_base_moof"])
        .arg("pipe:1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let spawn_start = Instant::now();
    let mut child = cmd.spawn()?;
    let ffmpeg_spawn_time = spawn_start.elapsed();

    let mut stdin = child.stdin.take().ok_or("ffmpeg stdin was not piped")?;
    let mut stdout = child.stdout.take().ok_or("ffmpeg stdout was not piped")?;
    let mut stderr = child.stderr.take().ok_or("ffmpeg stderr was not piped")?;
    // Disable Nagle so the final fragment is sent immediately rather than being
    // held waiting to coalesce — with `Connection: close` a delayed tail can
    // otherwise reach the client only at FIN, after playback has already ended.
    let _ = stream.set_nodelay(true);
    let mut stream_out = stream.try_clone()?;
    let client_alive = Arc::new(AtomicBool::new(true));
    let client_alive_for_stdout = Arc::clone(&client_alive);
    let collect_segment_body = segment_cache_key.is_some();

    // Drain ffmpeg's stdout to the client until TRUE EOF. ffmpeg emits the tail
    // GOP/fragments only after stdin is closed (the EOF below `drop(stdin)`), so
    // this thread must keep reading past the last frame the main loop wrote — it
    // is what carries the final ~GOP of frames to the client. A transient
    // `Interrupted` (EINTR) read is RETRIED, not treated as EOF: aborting on it
    // would truncate exactly that tail under load (the intermittent dropped-tail
    // bug). Only a real read error or a client write failure ends the drain.
    let stdout_thread = thread::spawn(move || {
        let mut buf = [0u8; 64 * 1024];
        let mut segment_body = collect_segment_body.then(Vec::new);
        loop {
            let n = match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    client_alive_for_stdout.store(false, Ordering::Relaxed);
                    break;
                }
            };
            if stream_out.write_all(&buf[..n]).is_err() {
                client_alive_for_stdout.store(false, Ordering::Relaxed);
                break;
            }
            if let Some(body) = &mut segment_body {
                body.extend_from_slice(&buf[..n]);
            }
        }
        // Flush the final fragments to the kernel before the connection's FIN so
        // the tail is not stranded in a userspace/socket buffer at close.
        let _ = stream_out.flush();
        segment_body
    });

    let stderr_thread = thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });

    let frame_step = 1.0 / setup.fps as f32;
    let frame_duration = Duration::from_secs_f32(frame_step);
    let mut frames_rendered = 0u64;
    let mut frames_written = 0u64;
    let mut render_total = Duration::ZERO;
    let mut stdin_write_total = Duration::ZERO;
    let mut end_reason = "complete";
    let cache_metrics_before = if setup.verbose {
        Some(
            app.lock()
                .map_err(|_| -> Box<dyn Error> { "preview app lock poisoned".into() })?
                .ctx
                .metrics(),
        )
    } else {
        None
    };

    for frame in 0..total_frames {
        if !client_alive.load(Ordering::Relaxed) {
            end_reason = "client_closed";
            client_alive.store(false, Ordering::Relaxed);
            break;
        }
        if video_epoch.load(Ordering::Acquire) != stream_epoch {
            end_reason = "superseded";
            client_alive.store(false, Ordering::Relaxed);
            break;
        }

        let frame_start = Instant::now();
        // Clamp into the half-open renderable range so the final frame of a
        // full-length stream (which can land on `total_duration`) renders the
        // last frame rather than erroring with `timeline did not produce a frame`.
        let seconds = clamp_to_renderable(
            setup.start_seconds + frame as f32 * frame_step,
            setup.total_duration,
            setup.fps,
        );
        let image = {
            let mut app = app
                .lock()
                .map_err(|_| -> Box<dyn Error> { "preview app lock poisoned".into() })?;
            let verbose = app.verbose;
            let frame = app.render_video_rgba(
                &setup.timeline_id,
                seconds,
                setup.resolution,
                setup.motion_blur,
                verbose,
            )?;
            if verbose {
                println!(
                    "video timeline={} t={:.3}s size={}x{} fps={} gop={} render={:.2}ms build={:.2}ms readback={:.2}ms bytes={} cache_delta={}h/{}m cache_size={} gpu_preference={} gpu_init_attempted={} gpu_init_error={} gpu_available={} gpu_ops={} gpu_readbacks={}",
                    setup.timeline_id,
                    seconds,
                    setup.resolution.width,
                    setup.resolution.height,
                    setup.fps,
                    setup.gop,
                    ms(frame.render_time),
                    ms(frame.build_time),
                    ms(frame.readback_time),
                    frame.image.pixels.len(),
                    frame.cache_hits,
                    frame.cache_misses,
                    format_bytes(frame.bytes_cached as u64),
                    frame.gpu_preference,
                    frame.gpu_init_attempted,
                    frame.gpu_init_error.as_deref().unwrap_or("-"),
                    frame.gpu_available,
                    frame.gpu_ops,
                    frame.gpu_readbacks,
                );
            }
            render_total += frame.render_time;
            frames_rendered += 1;
            frame.image
        };

        if video_epoch.load(Ordering::Acquire) != stream_epoch {
            end_reason = "superseded";
            client_alive.store(false, Ordering::Relaxed);
            break;
        }
        let write_start = Instant::now();
        let write_result = stdin.write_all(&image.pixels);
        stdin_write_total += write_start.elapsed();
        if write_result.is_err() {
            end_reason = "ffmpeg_stdin_closed";
            client_alive.store(false, Ordering::Relaxed);
            break;
        }
        frames_written += 1;
        if setup.realtime {
            sleep_remainder(frame_duration, frame_start.elapsed());
        }
    }

    drop(stdin);
    if !client_alive.load(Ordering::Relaxed) {
        let _ = child.kill();
    }
    let segment_body = stdout_thread.join().unwrap_or(None);
    if !client_alive.load(Ordering::Relaxed) && end_reason == "complete" {
        end_reason = "client_closed";
    }
    let stderr_text = stderr_thread.join().unwrap_or_default();
    let status = child.wait()?;
    if status.success() && client_alive.load(Ordering::Relaxed) && end_reason == "complete" {
        if let (Some(key), Some(body)) = (segment_cache_key, segment_body) {
            cache_video_segment(key, body);
        }
    }
    if setup.verbose {
        println!(
            "video-stream timeline={} start={:.3}s duration={:.3}s frames={}/{} written={} reason={} setup={:.2}ms audio={} audio_setup={:.2}ms ffmpeg_spawn={:.2}ms render_total={:.2}ms stdin_write={:.2}ms total={:.2}ms status={} stderr_bytes={}",
            setup.timeline_id,
            setup.start_seconds,
            video_seconds,
            frames_rendered,
            total_frames,
            frames_written,
            end_reason,
            ms(setup_time),
            audio_source,
            ms(audio_time),
            ms(ffmpeg_spawn_time),
            ms(render_total),
            ms(stdin_write_total),
            ms(stream_start.elapsed()),
            status,
            stderr_text.len(),
        );
        if let Some(before) = cache_metrics_before {
            if let Ok(app) = app.lock() {
                log_cache_metrics_delta(&before, &app.ctx.metrics());
            }
        }
    }
    if !status.success() && client_alive.load(Ordering::Relaxed) {
        return Err(format!("ffmpeg exited with {status}: {stderr_text}").into());
    }

    Ok(())
}

struct RenderedFrame {
    body: Vec<u8>,
    stats: FrameRenderStats,
}

struct RenderedImage {
    image: CpuRasterImage,
    stats: FrameRenderStats,
    total_start: Instant,
}

struct FrameRenderStats {
    timeline_id: String,
    seconds: f32,
    resolution: Resolution,
    render_time: Duration,
    encode_time: Duration,
    total_time: Duration,
    output_format: FrameFormat,
    output_bytes: usize,
    cache_hits: u64,
    cache_misses: u64,
    bytes_cached: usize,
    gpu_preference: String,
    gpu_init_attempted: bool,
    gpu_init_error: Option<String>,
    gpu_available: bool,
    gpu_ops: u64,
    gpu_readbacks: u64,
}

impl FrameRenderStats {
    fn headers(&self) -> Vec<(&'static str, String)> {
        let mut headers = vec![
            ("X-Tellur-Render-Ms", format!("{:.2}", ms(self.render_time))),
            ("X-Tellur-Encode-Ms", format!("{:.2}", ms(self.encode_time))),
            ("X-Tellur-Total-Ms", format!("{:.2}", ms(self.total_time))),
            (
                "X-Tellur-Output-Format",
                self.output_format.as_str().to_owned(),
            ),
            ("X-Tellur-Output-Bytes", self.output_bytes.to_string()),
            ("X-Tellur-Width", self.resolution.width.to_string()),
            ("X-Tellur-Height", self.resolution.height.to_string()),
            ("X-Tellur-Cache-Hits", self.cache_hits.to_string()),
            ("X-Tellur-Cache-Misses", self.cache_misses.to_string()),
            ("X-Tellur-GPU-Available", self.gpu_available.to_string()),
            (
                "X-Tellur-GPU-Init-Attempted",
                self.gpu_init_attempted.to_string(),
            ),
            ("X-Tellur-GPU-Preference", self.gpu_preference.clone()),
            ("X-Tellur-GPU-Active", (self.gpu_ops > 0).to_string()),
            ("X-Tellur-GPU-Ops", self.gpu_ops.to_string()),
            ("X-Tellur-GPU-Readbacks", self.gpu_readbacks.to_string()),
        ];
        if let Some(error) = &self.gpu_init_error {
            headers.push(("X-Tellur-GPU-Init-Error", sanitize_header_value(error)));
        }
        headers
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameFormat {
    Png,
    Rgba,
}

impl FrameFormat {
    fn from_query(query: &HashMap<String, String>) -> Self {
        match query.get("format").map(String::as_str) {
            Some("rgba") | Some("raw") => Self::Rgba,
            _ => Self::Png,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Rgba => "rgba",
        }
    }
}

fn log_frame_stats(stats: &FrameRenderStats) {
    println!(
        "frame timeline={} t={:.3}s size={}x{} format={} render={:.2}ms encode={:.2}ms total={:.2}ms bytes={} cache_delta={}h/{}m cache_size={} gpu_preference={} gpu_init_attempted={} gpu_init_error={} gpu_available={} gpu_ops={} gpu_readbacks={}",
        stats.timeline_id,
        stats.seconds,
        stats.resolution.width,
        stats.resolution.height,
        stats.output_format.as_str(),
        ms(stats.render_time),
        ms(stats.encode_time),
        ms(stats.total_time),
        stats.output_bytes,
        stats.cache_hits,
        stats.cache_misses,
        format_bytes(stats.bytes_cached as u64),
        stats.gpu_preference,
        stats.gpu_init_attempted,
        stats.gpu_init_error.as_deref().unwrap_or("-"),
        stats.gpu_available,
        stats.gpu_ops,
        stats.gpu_readbacks,
    );
}

#[derive(Clone, Copy)]
struct TypeStatsDelta {
    hits: u64,
    misses: u64,
    inclusive_time: Duration,
    self_time: Duration,
}

impl TypeStatsDelta {
    fn total(self) -> u64 {
        self.hits + self.misses
    }

    fn hit_rate(self) -> f64 {
        let total = self.total();
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

fn log_cache_metrics_delta(before: &CacheMetrics, after: &CacheMetrics) {
    let hits = after.hits.saturating_sub(before.hits);
    let misses = after.misses.saturating_sub(before.misses);
    let total = hits + misses;
    let hit_rate = if total == 0 {
        0.0
    } else {
        hits as f64 / total as f64
    };
    let gpu_before = &before.gpu;
    let gpu_after = &after.gpu;
    println!(
        "video-stream-cache-delta hits={} misses={} hit_rate={:.1}% cache_size={} evicted_delta={} pressure_skips_delta={} oversize_skips_delta={} admission_skips_delta={} budget_skips_delta={} gpu_ops={} gpu_composites={} gpu_shadows={} gpu_outlines={} gpu_rasterizes={} gpu_fills={} gpu_temporal_avg={} gpu_readbacks={}",
        hits,
        misses,
        hit_rate * 100.0,
        format_bytes(after.bytes_cached as u64),
        format_bytes(after.bytes_evicted.saturating_sub(before.bytes_evicted)),
        after.pressure_skips.saturating_sub(before.pressure_skips),
        after.oversize_skips.saturating_sub(before.oversize_skips),
        after.admission_skips.saturating_sub(before.admission_skips),
        after.budget_skips.saturating_sub(before.budget_skips),
        gpu_after.total_ops().saturating_sub(gpu_before.total_ops()),
        gpu_after.composites.saturating_sub(gpu_before.composites),
        gpu_after.drop_shadows.saturating_sub(gpu_before.drop_shadows),
        gpu_after.outlines.saturating_sub(gpu_before.outlines),
        gpu_after.rasterizes.saturating_sub(gpu_before.rasterizes),
        gpu_after.fills.saturating_sub(gpu_before.fills),
        gpu_after
            .temporal_averages
            .saturating_sub(gpu_before.temporal_averages),
        gpu_after.readbacks.saturating_sub(gpu_before.readbacks),
    );

    let mut rows: Vec<(&'static str, TypeStatsDelta)> = after
        .per_type
        .iter()
        .map(|(name, stats)| {
            let before_stats = before.per_type.get(name);
            (*name, diff_type_stats(before_stats, stats))
        })
        .filter(|(_, stats)| stats.total() > 0 || !stats.self_time.is_zero())
        .collect();
    rows.sort_by_key(|(_, stats)| Reverse(stats.self_time));
    for (name, stats) in rows.into_iter().take(12) {
        println!(
            "video-stream-cache-type name={} hits={} misses={} hit_rate={:.1}% self={} incl={}",
            name,
            stats.hits,
            stats.misses,
            stats.hit_rate() * 100.0,
            format_duration(stats.self_time),
            format_duration(stats.inclusive_time),
        );
    }
}

fn diff_type_stats(before: Option<&TypeStats>, after: &TypeStats) -> TypeStatsDelta {
    let before = before.copied().unwrap_or_default();
    TypeStatsDelta {
        hits: after.hits.saturating_sub(before.hits),
        misses: after.misses.saturating_sub(before.misses),
        inclusive_time: after.inclusive_time.saturating_sub(before.inclusive_time),
        self_time: after.self_time.saturating_sub(before.self_time),
    }
}

fn format_duration(d: Duration) -> String {
    let micros = d.as_micros();
    if micros >= 1_000_000 {
        format!("{:.2}s", d.as_secs_f64())
    } else if micros >= 1_000 {
        format!("{:.2}ms", micros as f64 / 1_000.0)
    } else {
        format!("{micros}us")
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn sleep_remainder(frame_duration: Duration, elapsed: Duration) {
    if let Some(remaining) = frame_duration.checked_sub(elapsed) {
        thread::sleep(remaining);
    }
}

fn select_timeline<'a>(
    timelines: &'a [TimelineInfo],
    requested: Option<&String>,
) -> Option<&'a TimelineInfo> {
    requested
        .and_then(|id| timelines.iter().find(|info| &info.id == id))
        .or_else(|| timelines.first())
}

fn request_fps(query: &HashMap<String, String>, default_fps: u32) -> u32 {
    query
        .get("fps")
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|fps| *fps > 0)
        .unwrap_or(default_fps.max(1))
}

/// The preview's motion-blur toggle; off unless the request explicitly opts in.
fn request_motion_blur(query: &HashMap<String, String>) -> bool {
    matches!(
        query.get("motion_blur").map(String::as_str),
        Some("1") | Some("true")
    )
}

fn request_color_range(query: &HashMap<String, String>, default: ColorRange) -> ColorRange {
    query
        .get("color_range")
        .or_else(|| query.get("colorRange"))
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn request_video_color(query: &HashMap<String, String>) -> bool {
    matches!(
        query
            .get("video_color")
            .or_else(|| query.get("videoColor"))
            .map(String::as_str),
        Some("1") | Some("true") | Some("mp4") | Some("video")
    )
}

fn request_resolution(query: &HashMap<String, String>, default: Resolution) -> Resolution {
    if let (Some(width), Some(height)) = (
        query
            .get("width")
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v > 0),
        query
            .get("height")
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v > 0),
    ) {
        return Resolution::new(width, height);
    }

    let Some(scale) = query
        .get("scale")
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
    else {
        return default;
    };

    Resolution::new(
        scaled_dimension(default.width, scale),
        scaled_dimension(default.height, scale),
    )
}

fn scaled_dimension(value: u32, scale: f32) -> u32 {
    ((value as f32) * scale).round().clamp(1.0, u32::MAX as f32) as u32
}

fn video_color_preview_image(
    image: &CpuRasterImage,
    color_range: ColorRange,
) -> Result<CpuRasterImage, Box<dyn Error>> {
    if image.format != PixelFormat::Rgba8 {
        return Err(format!("video-color preview requires Rgba8, got {:?}", image.format).into());
    }

    let width = image.width as usize;
    let height = image.height as usize;
    let expected = width * height * 4;
    if image.pixels.len() != expected {
        return Err(format!(
            "video-color frame size mismatch: expected {expected} bytes, got {}",
            image.pixels.len()
        )
        .into());
    }

    let mut out = vec![0u8; expected];
    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            let mut chroma = [(0usize, 0.0_f32, 0.0_f32, 0.0_f32); 4];
            let mut count = 0usize;
            for dy in 0..2 {
                let py = y + dy;
                if py >= height {
                    continue;
                }
                for dx in 0..2 {
                    let px = x + dx;
                    if px >= width {
                        continue;
                    }
                    let idx = (py * width + px) * 4;
                    let rgb = [
                        image.pixels[idx] as f32,
                        image.pixels[idx + 1] as f32,
                        image.pixels[idx + 2] as f32,
                    ];
                    let (encoded_y, encoded_cb, encoded_cr) = bt709_rgb_to_ycbcr(rgb, color_range);
                    chroma[count] = (idx, encoded_y, encoded_cb, encoded_cr);
                    count += 1;
                }
            }
            if count == 0 {
                continue;
            }

            let cb = quantize_u8(
                chroma[..count].iter().map(|(_, _, cb, _)| *cb).sum::<f32>() / count as f32,
            ) as f32;
            let cr = quantize_u8(
                chroma[..count].iter().map(|(_, _, _, cr)| *cr).sum::<f32>() / count as f32,
            ) as f32;
            for &(idx, encoded_y, _, _) in &chroma[..count] {
                let yy = quantize_u8(encoded_y) as f32;
                let [r, g, b] = bt709_ycbcr_to_rgb(yy, cb, cr, color_range);
                out[idx] = quantize_u8(r);
                out[idx + 1] = quantize_u8(g);
                out[idx + 2] = quantize_u8(b);
                out[idx + 3] = image.pixels[idx + 3];
            }
        }
    }

    Ok(CpuRasterImage::new(
        image.width,
        image.height,
        PixelFormat::Rgba8,
        out,
    ))
}

fn bt709_rgb_to_ycbcr(rgb: [f32; 3], color_range: ColorRange) -> (f32, f32, f32) {
    let [r, g, b] = rgb;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let cb = (b - y) / 1.8556;
    let cr = (r - y) / 1.5748;
    match color_range {
        ColorRange::Full => (y, 128.0 + cb, 128.0 + cr),
        ColorRange::Limited => (
            16.0 + y * (219.0 / 255.0),
            128.0 + cb * (224.0 / 255.0),
            128.0 + cr * (224.0 / 255.0),
        ),
    }
}

fn bt709_ycbcr_to_rgb(y: f32, cb: f32, cr: f32, color_range: ColorRange) -> [f32; 3] {
    let (y, cb, cr) = match color_range {
        ColorRange::Full => (y, cb - 128.0, cr - 128.0),
        ColorRange::Limited => (
            (y - 16.0) * (255.0 / 219.0),
            (cb - 128.0) * (255.0 / 224.0),
            (cr - 128.0) * (255.0 / 224.0),
        ),
    };
    [
        y + 1.5748 * cr,
        y - 0.187_324 * cb - 0.468_124 * cr,
        y + 1.8556 * cb,
    ]
}

fn quantize_u8(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

fn export_preview_png<W: Write>(image: &CpuRasterImage, writer: W) -> Result<(), Box<dyn Error>> {
    if image.format != PixelFormat::Rgba8 {
        return Err(format!("png frame requires Rgba8, got {:?}", image.format).into());
    }

    let expected = (image.width as usize) * (image.height as usize) * 4;
    if image.pixels.len() != expected {
        return Err(format!(
            "png frame size mismatch: expected {expected} bytes, got {}",
            image.pixels.len()
        )
        .into());
    }

    let mut encoder = png::Encoder::new(writer, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fastest);
    let mut png_writer = encoder.write_header()?;
    png_writer.write_image_data(&image.pixels)?;
    Ok(())
}

/// Clamps a requested frame time into the timeline's RENDERABLE range.
///
/// Clip time gates in the core are half-open `[start, end)` (`tellur-core`
/// `timeline_component.rs`'s `t < end`), so a request at exactly `duration`
/// leaves every clip inactive and the composite is `None` — surfacing as a
/// `timeline did not produce a frame` 500. A frontend scrubbing to the very end
/// (or any `time=<duration>` request) would otherwise break.
///
/// So the upper bound is the LAST renderable frame time, one frame step below
/// `duration` (`(duration - 1/fps).max(0.0)`), which satisfies `t < duration`
/// and lands in the final frame's interval. A zero/sub-frame timeline clamps to
/// `0.0`. This is the server-side root guard: an end-of-timeline request returns
/// the last frame instead of erroring.
fn clamp_to_renderable(seconds: f32, duration: f32, fps: u32) -> f32 {
    let frame_step = 1.0 / fps.max(1) as f32;
    let last_frame = (duration - frame_step).max(0.0);
    seconds.clamp(0.0, last_frame)
}

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
}

fn read_request(stream: &mut TcpStream) -> Result<Option<Request>, Box<dyn Error>> {
    let mut buf = Vec::with_capacity(8192);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(None);
            }
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 64 * 1024 {
            break;
        }
    }

    let request = String::from_utf8_lossy(&buf);
    let first_line = request.lines().next().ok_or("empty request")?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next().ok_or("missing method")?.to_owned();
    let target = parts.next().ok_or("missing request target")?;
    let (path, query) = split_target(target);
    Ok(Some(Request {
        method,
        path,
        query,
    }))
}

fn split_target(target: &str) -> (String, HashMap<String, String>) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut params = HashMap::new();
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(percent_decode(k), percent_decode(v));
    }
    (path.to_owned(), params)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                    if let Ok(v) = u8::from_str_radix(hex, 16) {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                }
                out.push(bytes[i]);
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> Result<(), Box<dyn Error>> {
    write_response_with_headers(stream, status, reason, content_type, &[], body)
}

fn write_response_with_headers(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    extra_headers: &[(&str, String)],
    body: &[u8],
) -> Result<(), Box<dyn Error>> {
    write_response_with_headers_and_cache_control(
        stream,
        status,
        reason,
        content_type,
        extra_headers,
        body,
        "no-store",
    )
}

fn write_response_with_headers_and_cache_control(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    extra_headers: &[(&str, String)],
    body: &[u8],
    cache_control: &str,
) -> Result<(), Box<dyn Error>> {
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: {cache_control}\r\n\
         Connection: close\r\n",
        body.len()
    )?;
    for (name, value) in extra_headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    stream.write_all(b"\r\n")?;
    stream.write_all(body)?;
    Ok(())
}

fn format_bytes(b: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bf = b as f64;
    if bf >= GIB {
        format!("{:.2} GiB", bf / GIB)
    } else if bf >= MIB {
        format!("{:.2} MiB", bf / MIB)
    } else if bf >= KIB {
        format!("{:.2} KiB", bf / KIB)
    } else {
        format!("{b} B")
    }
}

fn sanitize_header_value(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn info_json(
    project_name: &str,
    resolution: Resolution,
    fps: u32,
    timelines: &[TimelineInfo],
    last_error: Option<&str>,
    cache_key: &str,
    compile: &CompileSnapshot,
) -> String {
    let timelines_json = timelines
        .iter()
        .map(|info| {
            let error = match info.error.as_deref() {
                Some(e) => format!("\"{}\"", json_escape(e)),
                None => "null".to_owned(),
            };
            format!(
                "{{\"id\":\"{}\",\"title\":\"{}\",\"duration\":{},\"error\":{}}}",
                json_escape(&info.id),
                json_escape(&info.title),
                finite_json_number(info.duration),
                error,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let last_error = match last_error {
        Some(e) => format!("\"{}\"", json_escape(e)),
        None => "null".to_owned(),
    };
    let compile_error = match compile.last_error.as_deref() {
        Some(e) => format!("\"{}\"", json_escape(e)),
        None => "null".to_owned(),
    };
    format!(
        "{{\"projectName\":\"{}\",\"width\":{},\"height\":{},\"fps\":{},\"lastError\":{},\"cacheKey\":\"{}\",\"compileStatus\":\"{}\",\"compileError\":{},\"timelines\":[{}]}}",
        json_escape(project_name),
        resolution.width,
        resolution.height,
        fps,
        last_error,
        json_escape(cache_key),
        compile.status.as_str(),
        compile_error,
        timelines_json
    )
}

/// The lowercased `NodeKind` discriminant the live UI keys its rendering on.
/// Kept in lock-step with `web/src/types.ts`'s `NodeKind` union.
fn node_kind_str(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Video => "video",
        NodeKind::Audio => "audio",
        NodeKind::Subtitle => "subtitle",
        NodeKind::Timeline => "timeline",
        NodeKind::Sequence => "sequence",
    }
}

/// Serializes an [`Arrangement`] node and its children into the hand-built JSON
/// the live UI consumes (audit B.4). Recurses over `children`; every float goes
/// through [`finite_json_number`]; `trim` is `null` or `[a,b]`; each trigger is
/// an object `{"time":<num>,"name":<null|string>}`; `source` is `null` or
/// `{"file":<string>,"line":<num>}`. Shape mirrored in `web/src/types.ts`.
fn arrangement_json(node: &Arrangement) -> String {
    let trim = match node.trim {
        Some((a, b)) => format!("[{},{}]", finite_json_number(a), finite_json_number(b)),
        None => "null".to_owned(),
    };
    let triggers = node
        .triggers
        .iter()
        .map(|t| {
            let name = match &t.name {
                Some(n) => format!("\"{}\"", json_escape(n)),
                None => "null".to_owned(),
            };
            format!(
                "{{\"time\":{},\"name\":{}}}",
                finite_json_number(t.time),
                name,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let children = node
        .children
        .iter()
        .map(arrangement_json)
        .collect::<Vec<_>>()
        .join(",");
    let name = match &node.name {
        Some(n) => format!("\"{}\"", json_escape(n)),
        None => "null".to_owned(),
    };
    let source = match &node.source {
        Some(s) => format!(
            "{{\"file\":\"{}\",\"line\":{}}}",
            json_escape(&s.file),
            s.line,
        ),
        None => "null".to_owned(),
    };
    format!(
        "{{\"kind\":\"{}\",\"label\":\"{}\",\"name\":{},\"source\":{},\"start\":{},\"end\":{},\"trim\":{},\"triggers\":[{}],\"children\":[{}]}}",
        node_kind_str(node.kind),
        json_escape(&node.label),
        name,
        source,
        finite_json_number(node.start),
        finite_json_number(node.end),
        trim,
        triggers,
        children,
    )
}

fn finite_json_number(v: f32) -> String {
    if v.is_finite() {
        v.to_string()
    } else {
        "0".to_owned()
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tellur_core::timeline_component::{SourceLoc, TriggerMark};

    #[test]
    fn temp_wav_uses_f32le_and_preserves_headroom() {
        let buf = AudioBuffer {
            samples: vec![1.5, -2.0],
            rate: 48_000,
            channels: 1,
        };
        let path = write_temp_wav(&buf).expect("write temp float wav");
        let bytes = std::fs::read(&path).expect("read temp float wav");

        assert_eq!(&bytes[20..22], &3u16.to_le_bytes());
        assert_eq!(&bytes[34..36], &32u16.to_le_bytes());
        assert_eq!(&bytes[40..44], &8u32.to_le_bytes());
        assert_eq!(&bytes[44..48], &1.5_f32.to_le_bytes());
        assert_eq!(&bytes[48..52], &(-2.0_f32).to_le_bytes());

        let _ = std::fs::remove_file(path);
    }

    // A request for exactly `duration` must clamp into the half-open renderable
    // range: `< duration` (so the core's `t < end` clip gate stays active) AND
    // inside the LAST frame's interval `[duration - 1/fps, duration)`. This is
    // the root guard for "scrub to the very end" — without it `t == duration`
    // leaves every clip inactive and the composite is `None` (a 500
    // `timeline did not produce a frame`).
    #[test]
    fn clamp_to_renderable_maps_exact_duration_to_last_frame() {
        let duration = 7.6_f32;
        let fps = 60;
        let frame_step = 1.0 / fps as f32;

        let clamped = clamp_to_renderable(duration, duration, fps);
        // Strictly inside the timeline (the half-open endpoint is excluded).
        assert!(clamped < duration, "{clamped} must be < {duration}");
        // And within the final frame's interval, so it renders the last frame.
        assert!(
            clamped >= duration - frame_step,
            "{clamped} must be in the last frame interval [{}, {duration})",
            duration - frame_step
        );
        // Exactly the last frame time (one step below duration).
        assert!((clamped - (duration - frame_step)).abs() < 1e-6);

        // A past-the-end request clamps to the same last frame.
        assert_eq!(clamp_to_renderable(duration + 5.0, duration, fps), clamped);
    }

    #[test]
    fn clamp_to_renderable_passes_through_interior_times() {
        // A time comfortably inside the range is unchanged.
        let t = clamp_to_renderable(3.0, 7.6, 60);
        assert_eq!(t, 3.0);
    }

    #[test]
    fn clamp_to_renderable_handles_short_and_negative() {
        // A timeline shorter than one frame (or zero) clamps to 0.0 rather than
        // going negative.
        assert_eq!(clamp_to_renderable(1.0, 0.0, 60), 0.0);
        assert_eq!(clamp_to_renderable(0.005, 0.01, 60), 0.0);
        // A negative request floors at 0.0.
        assert_eq!(clamp_to_renderable(-2.0, 7.6, 60), 0.0);
    }

    #[test]
    fn request_motion_blur_defaults_off() {
        assert!(!request_motion_blur(&HashMap::new()));

        let mut query = HashMap::new();
        query.insert("motion_blur".to_owned(), "0".to_owned());
        assert!(!request_motion_blur(&query));

        query.insert("motion_blur".to_owned(), "false".to_owned());
        assert!(!request_motion_blur(&query));
    }

    #[test]
    fn request_motion_blur_is_explicitly_opt_in() {
        let mut query = HashMap::new();
        query.insert("motion_blur".to_owned(), "1".to_owned());
        assert!(request_motion_blur(&query));

        query.insert("motion_blur".to_owned(), "true".to_owned());
        assert!(request_motion_blur(&query));
    }

    #[test]
    fn request_color_range_defaults_to_server_value() {
        assert_eq!(
            request_color_range(&HashMap::new(), ColorRange::Limited),
            ColorRange::Limited
        );

        let mut query = HashMap::new();
        query.insert("color_range".to_owned(), "bogus".to_owned());
        assert_eq!(
            request_color_range(&query, ColorRange::Full),
            ColorRange::Full
        );
    }

    #[test]
    fn request_color_range_accepts_query_aliases() {
        let mut query = HashMap::new();
        query.insert("color_range".to_owned(), "limited".to_owned());
        assert_eq!(
            request_color_range(&query, ColorRange::Full),
            ColorRange::Limited
        );

        query.clear();
        query.insert("colorRange".to_owned(), "pc".to_owned());
        assert_eq!(
            request_color_range(&query, ColorRange::Limited),
            ColorRange::Full
        );
    }

    #[test]
    fn request_video_color_is_explicitly_opt_in() {
        assert!(!request_video_color(&HashMap::new()));

        let mut query = HashMap::new();
        query.insert("video_color".to_owned(), "1".to_owned());
        assert!(request_video_color(&query));

        query.clear();
        query.insert("videoColor".to_owned(), "mp4".to_owned());
        assert!(request_video_color(&query));
    }

    #[test]
    fn video_color_preview_preserves_gray_pixels() {
        let image = CpuRasterImage::new(
            2,
            2,
            PixelFormat::Rgba8,
            vec![
                64, 64, 64, 255, 128, 128, 128, 200, 200, 200, 200, 180, 255, 255, 255, 128,
            ],
        );

        let out = video_color_preview_image(&image, ColorRange::Full).expect("convert");
        assert_eq!(out.pixels, image.pixels);
    }

    #[test]
    fn video_color_preview_shares_chroma_per_420_block() {
        let image =
            CpuRasterImage::new(2, 1, PixelFormat::Rgba8, vec![255, 0, 0, 77, 0, 0, 255, 88]);

        let out = video_color_preview_image(&image, ColorRange::Full).expect("convert");
        assert_eq!(out.width, 2);
        assert_eq!(out.height, 1);
        assert_eq!(out.format, PixelFormat::Rgba8);
        assert_eq!(out.pixels[3], 77);
        assert_eq!(out.pixels[7], 88);
        assert_ne!(&out.pixels[..3], &image.pixels[..3]);
        assert_ne!(&out.pixels[4..7], &image.pixels[4..7]);
    }

    #[test]
    fn info_json_includes_the_project_name() {
        let timelines = vec![TimelineInfo {
            id: "main".to_owned(),
            title: "Main".to_owned(),
            duration: 4.0,
            error: None,
        }];
        let json = info_json(
            "demo \"crate\"",
            Resolution::new(1280, 720),
            30,
            &timelines,
            None,
            "cache-key",
            &CompileSnapshot::compiled(),
        );

        assert_eq!(
            json,
            "{\"projectName\":\"demo \\\"crate\\\"\",\"width\":1280,\"height\":720,\"fps\":30,\"lastError\":null,\"cacheKey\":\"cache-key\",\"compileStatus\":\"compiled\",\"compileError\":null,\"timelines\":[{\"id\":\"main\",\"title\":\"Main\",\"duration\":4,\"error\":null}]}"
        );
    }

    // Round-trips the `.sketch/01` B.4 arrangement shape at a small scale: an
    // overlay timeline with a video child (a source crop) and a sequence of two
    // captions, one carrying a trigger. Asserts the hand-built emitter matches
    // the documented JSON exactly (lowercased kinds, `null`/`[a,b]` trim, every
    // float through `finite_json_number`, recursive children).
    #[test]
    fn arrangement_json_matches_the_b4_shape() {
        let arrangement = Arrangement {
            kind: NodeKind::Timeline,
            label: "root".to_owned(),
            // A non-null `name` exercises the escaped-string branch; the rest stay
            // `null` to cover the absent-name branch.
            name: Some("Dialogue · \"hi\"".to_owned()),
            source: None,
            start: 0.0,
            end: 6.0,
            trim: None,
            triggers: Vec::new(),
            children: vec![
                Arrangement {
                    kind: NodeKind::Video,
                    label: "establishing.mp4".to_owned(),
                    name: None,
                    // A non-null `source` exercises the object branch (note the
                    // backslash escaping in the file path); the rest stay `null`.
                    source: Some(SourceLoc {
                        file: "scenes\\intro.rs".to_owned(),
                        line: 42,
                    }),
                    start: 0.0,
                    end: 2.0,
                    trim: Some((1.0, 3.0)),
                    triggers: Vec::new(),
                    children: Vec::new(),
                },
                Arrangement {
                    kind: NodeKind::Sequence,
                    label: String::new(),
                    name: None,
                    source: None,
                    start: 0.0,
                    end: 6.0,
                    trim: None,
                    triggers: Vec::new(),
                    children: vec![
                        Arrangement {
                            kind: NodeKind::Video,
                            label: "one".to_owned(),
                            name: None,
                            source: None,
                            start: 0.0,
                            end: 3.0,
                            trim: None,
                            triggers: Vec::new(),
                            children: Vec::new(),
                        },
                        Arrangement {
                            kind: NodeKind::Video,
                            label: "two".to_owned(),
                            name: None,
                            source: None,
                            start: 3.0,
                            end: 6.0,
                            trim: None,
                            // A named trigger exercises the string branch; an
                            // anonymous one covers the `null` branch.
                            triggers: vec![
                                TriggerMark {
                                    time: 3.0,
                                    name: Some("reveal".to_owned()),
                                },
                                TriggerMark {
                                    time: 4.0,
                                    name: None,
                                },
                            ],
                            children: Vec::new(),
                        },
                    ],
                },
            ],
        };

        let expected = concat!(
            "{\"kind\":\"timeline\",\"label\":\"root\",\"name\":\"Dialogue · \\\"hi\\\"\",\"source\":null,\"start\":0,\"end\":6,",
            "\"trim\":null,\"triggers\":[],\"children\":[",
            "{\"kind\":\"video\",\"label\":\"establishing.mp4\",\"name\":null,\"source\":{\"file\":\"scenes\\\\intro.rs\",\"line\":42},\"start\":0,\"end\":2,",
            "\"trim\":[1,3],\"triggers\":[],\"children\":[]},",
            "{\"kind\":\"sequence\",\"label\":\"\",\"name\":null,\"source\":null,\"start\":0,\"end\":6,",
            "\"trim\":null,\"triggers\":[],\"children\":[",
            "{\"kind\":\"video\",\"label\":\"one\",\"name\":null,\"source\":null,\"start\":0,\"end\":3,",
            "\"trim\":null,\"triggers\":[],\"children\":[]},",
            "{\"kind\":\"video\",\"label\":\"two\",\"name\":null,\"source\":null,\"start\":3,\"end\":6,",
            "\"trim\":null,\"triggers\":[",
            "{\"time\":3,\"name\":\"reveal\"},",
            "{\"time\":4,\"name\":null}",
            "],\"children\":[]}",
            "]}",
            "]}"
        );

        assert_eq!(arrangement_json(&arrangement), expected);
    }

    #[test]
    fn arrangement_json_non_finite_floats_become_zero() {
        // `finite_json_number` guards every float, so an unfired/absent length
        // (∞ / NaN) never leaks a non-JSON token.
        let arrangement = Arrangement {
            kind: NodeKind::Video,
            label: String::new(),
            name: None,
            source: None,
            start: f32::INFINITY,
            end: f32::NAN,
            trim: None,
            triggers: vec![TriggerMark {
                time: f32::INFINITY,
                name: None,
            }],
            children: Vec::new(),
        };
        let json = arrangement_json(&arrangement);
        assert!(json.contains("\"start\":0"));
        assert!(json.contains("\"end\":0"));
        assert!(json.contains("\"triggers\":[{\"time\":0,\"name\":null}]"));
        assert!(json.contains("\"source\":null"));
    }
}
