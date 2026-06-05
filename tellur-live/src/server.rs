use std::collections::HashMap;
use std::error::Error;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};

use tellur_core::raster::{CpuRasterImage, PixelFormat, Resolution};
use tellur_core::render_context::{GpuPreference, RenderContext};
use tellur_core::time::TimelineTime;
use tellur_core::timeline_component::{Arrangement, NodeKind};
use tellur_renderer::CachingRenderContext;

use crate::build_watch::{
    run_release_build_once, start_build_watcher, AutoBuildOptions, CompileSnapshot, CompileState,
};
use crate::plugin::{HotReloadPlugin, TimelineInfo};

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub plugin_path: PathBuf,
    pub bind: String,
    pub resolution: Resolution,
    pub fps: u32,
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
        eprintln!(
            "auto build: cargo build --release{} --example {}",
            auto_build
                .package
                .as_ref()
                .map(|package| format!(" -p {package}"))
                .unwrap_or_default(),
            auto_build.example
        );
        eprintln!("running initial release build");
        run_release_build_once(auto_build).map_err(|e| -> Box<dyn Error> { e.into() })?;
    }

    let compile_state = options
        .auto_build
        .clone()
        .map(start_build_watcher)
        .unwrap_or_else(CompileState::compiled);

    let app = Arc::new(Mutex::new(PreviewApp {
        plugin: HotReloadPlugin::new(options.plugin_path),
        ctx: CachingRenderContext::new().with_gpu_preference(options.gpu_preference),
        resolution: options.resolution,
        fps: options.fps,
        verbose: options.verbose,
        compile_state,
    }));
    {
        let mut app = app
            .lock()
            .map_err(|_| -> Box<dyn Error> { "preview app lock poisoned".into() })?;
        app.reload_plugin_if_changed()?;
    }

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let app = Arc::clone(&app);
                thread::spawn(move || {
                    if let Err(e) = handle_connection(app, stream) {
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

fn handle_connection(
    app: Arc<Mutex<PreviewApp>>,
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
        "/api/video.mp4" | "/api/video" => handle_video_stream(app, stream, request.query),
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
    ctx: CachingRenderContext,
    resolution: Resolution,
    fps: u32,
    verbose: bool,
    compile_state: CompileState,
}

impl PreviewApp {
    fn reload_plugin_if_changed(&mut self) -> Result<bool, Box<dyn Error>> {
        let changed = self.plugin.reload_if_changed()?;
        if changed {
            self.ctx.clear();
            self.ctx.clear_metrics();
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
    ) -> Result<VideoFrame, Box<dyn Error>> {
        let before = self.ctx.metrics();
        let render_start = Instant::now();
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
        let image = self.ctx.readback(image);
        let render_time = render_start.elapsed();
        if image.format != PixelFormat::Rgba8 {
            return Err(format!("h264 stream requires Rgba8, got {:?}", image.format).into());
        }
        let after = self.ctx.metrics();

        Ok(VideoFrame {
            image,
            render_time,
            cache_hits: after.hits.saturating_sub(before.hits),
            cache_misses: after.misses.saturating_sub(before.misses),
            bytes_cached: after.bytes_cached,
            gpu_available: after.gpu_available,
            gpu_ops: after.gpu.total_ops().saturating_sub(before.gpu.total_ops()),
            gpu_readbacks: after.gpu.readbacks.saturating_sub(before.gpu.readbacks),
        })
    }

    fn render_png(
        &mut self,
        query: &HashMap<String, String>,
    ) -> Result<RenderedFrame, Box<dyn Error>> {
        let rendered = self.render_image(query)?;

        let encode_start = Instant::now();
        let mut body = Vec::new();
        rendered.image.export_png(&mut body)?;
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
    start_seconds: f32,
    cache_control: &'static str,
    realtime: bool,
}

struct VideoFrame {
    image: CpuRasterImage,
    render_time: Duration,
    cache_hits: u64,
    cache_misses: u64,
    bytes_cached: usize,
    gpu_available: bool,
    gpu_ops: u64,
    gpu_readbacks: u64,
}

fn handle_video_stream(
    app: Arc<Mutex<PreviewApp>>,
    mut stream: TcpStream,
    query: HashMap<String, String>,
) -> Result<(), Box<dyn Error>> {
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
            start_seconds,
            cache_control: if cacheable {
                "public, max-age=31536000, immutable"
            } else {
                "no-store"
            },
            realtime: !cacheable,
        }
    };

    write!(
        stream,
        "HTTP/1.1 200 OK\r\n\
         Content-Type: video/mp4\r\n\
         X-Tellur-Width: {}\r\n\
         X-Tellur-Height: {}\r\n\
         X-Tellur-Fps: {}\r\n\
         X-Tellur-Gop: {}\r\n\
         Cache-Control: {}\r\n\
         Connection: close\r\n\r\n",
        setup.resolution.width, setup.resolution.height, setup.fps, setup.gop, setup.cache_control,
    )?;

    let mut child = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .args(["-f", "rawvideo"])
        .args(["-pix_fmt", "rgba"])
        .args([
            "-s",
            &format!("{}x{}", setup.resolution.width, setup.resolution.height),
        ])
        .args(["-r", &setup.fps.to_string()])
        .args(["-i", "-"])
        .arg("-an")
        .args(["-c:v", "libx264"])
        .args(["-preset", "ultrafast"])
        .args(["-tune", "zerolatency"])
        .args(["-pix_fmt", "yuv420p"])
        .args(["-g", &setup.gop.to_string()])
        .args(["-keyint_min", &setup.gop.to_string()])
        .args(["-sc_threshold", "0"])
        .args(["-bf", "0"])
        .args(["-refs", "1"])
        .args(["-flags", "low_delay"])
        .args(["-crf", &setup.crf.to_string()])
        .args(["-muxdelay", "0"])
        .args(["-muxpreload", "0"])
        .args(["-flush_packets", "1"])
        .args(["-f", "mp4"])
        .args([
            "-movflags",
            "frag_keyframe+frag_every_frame+empty_moov+default_base_moof",
        ])
        .arg("pipe:1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

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

    // Drain ffmpeg's stdout to the client until TRUE EOF. ffmpeg emits the tail
    // GOP/fragments only after stdin is closed (the EOF below `drop(stdin)`), so
    // this thread must keep reading past the last frame the main loop wrote — it
    // is what carries the final ~GOP of frames to the client. A transient
    // `Interrupted` (EINTR) read is RETRIED, not treated as EOF: aborting on it
    // would truncate exactly that tail under load (the intermittent dropped-tail
    // bug). Only a real read error or a client write failure ends the drain.
    let stdout_thread = thread::spawn(move || {
        let mut buf = [0u8; 64 * 1024];
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
        }
        // Flush the final fragments to the kernel before the connection's FIN so
        // the tail is not stranded in a userspace/socket buffer at close.
        let _ = stream_out.flush();
    });

    let stderr_thread = thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });

    let frame_step = 1.0 / setup.fps as f32;
    let frame_duration = Duration::from_secs_f32(frame_step);
    let total_frames = (setup.duration * setup.fps as f32).ceil() as u64;

    for frame in 0..total_frames {
        if !client_alive.load(Ordering::Relaxed) {
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
            let frame = app.render_video_rgba(&setup.timeline_id, seconds, setup.resolution)?;
            if app.verbose {
                println!(
                    "video timeline={} t={:.3}s size={}x{} fps={} gop={} render={:.2}ms bytes={} cache_delta={}h/{}m cache_size={} gpu_available={} gpu_ops={} gpu_readbacks={}",
                    setup.timeline_id,
                    seconds,
                    setup.resolution.width,
                    setup.resolution.height,
                    setup.fps,
                    setup.gop,
                    ms(frame.render_time),
                    frame.image.pixels.len(),
                    frame.cache_hits,
                    frame.cache_misses,
                    format_bytes(frame.bytes_cached as u64),
                    frame.gpu_available,
                    frame.gpu_ops,
                    frame.gpu_readbacks,
                );
            }
            frame.image
        };

        if stdin.write_all(&image.pixels).is_err() {
            client_alive.store(false, Ordering::Relaxed);
            break;
        }
        if setup.realtime {
            sleep_remainder(frame_duration, frame_start.elapsed());
        }
    }

    drop(stdin);
    if !client_alive.load(Ordering::Relaxed) {
        let _ = child.kill();
    }
    let _ = stdout_thread.join();
    let stderr_text = stderr_thread.join().unwrap_or_default();
    let status = child.wait()?;
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
    gpu_available: bool,
    gpu_ops: u64,
    gpu_readbacks: u64,
}

impl FrameRenderStats {
    fn headers(&self) -> Vec<(&'static str, String)> {
        vec![
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
            ("X-Tellur-GPU-Active", (self.gpu_ops > 0).to_string()),
            ("X-Tellur-GPU-Ops", self.gpu_ops.to_string()),
            ("X-Tellur-GPU-Readbacks", self.gpu_readbacks.to_string()),
        ]
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
        "frame timeline={} t={:.3}s size={}x{} format={} render={:.2}ms encode={:.2}ms total={:.2}ms bytes={} cache_delta={}h/{}m cache_size={} gpu_available={} gpu_ops={} gpu_readbacks={}",
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
        stats.gpu_available,
        stats.gpu_ops,
        stats.gpu_readbacks,
    );
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

fn info_json(
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
        "{{\"width\":{},\"height\":{},\"fps\":{},\"lastError\":{},\"cacheKey\":\"{}\",\"compileStatus\":\"{}\",\"compileError\":{},\"timelines\":[{}]}}",
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
