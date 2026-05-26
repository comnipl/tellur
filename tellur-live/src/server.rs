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

use tellur_core::raster::{PixelFormat, RasterImage, Resolution};
use tellur_core::time::TimelineTime;
use tellur_renderer::CachingRenderContext;

use crate::plugin::{HotReloadPlugin, TimelineInfo};

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub plugin_path: PathBuf,
    pub bind: String,
    pub resolution: Resolution,
    pub fps: u32,
    pub verbose: bool,
}

pub fn serve(options: ServerOptions) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind(&options.bind)?;
    let local_addr = listener.local_addr()?;
    eprintln!("tellur live listening on http://{local_addr}");
    eprintln!("plugin: {}", options.plugin_path.display());

    let app = Arc::new(Mutex::new(PreviewApp {
        plugin: HotReloadPlugin::new(options.plugin_path),
        ctx: CachingRenderContext::new(),
        resolution: options.resolution,
        fps: options.fps,
        verbose: options.verbose,
    }));
    {
        let mut app = app
            .lock()
            .map_err(|_| -> Box<dyn Error> { "preview app lock poisoned".into() })?;
        app.plugin.reload_if_changed()?;
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
        "/" | "/index.html" => write_response(
            &mut stream,
            200,
            "OK",
            "text/html; charset=utf-8",
            INDEX_HTML.as_bytes(),
        ),
        "/api/video.mp4" | "/api/video" => handle_video_stream(app, stream, request.query),
        "/api/info" | "/api/frame" | "/api/stream" => {
            let mut app = app
                .lock()
                .map_err(|_| -> Box<dyn Error> { "preview app lock poisoned".into() })?;
            app.handle_api(stream, request)
        }
        _ => write_response(
            &mut stream,
            404,
            "Not Found",
            "text/plain; charset=utf-8",
            b"not found",
        ),
    }
}

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
}

impl PreviewApp {
    fn handle_api(&mut self, stream: TcpStream, request: Request) -> Result<(), Box<dyn Error>> {
        match request.path.as_str() {
            "/api/info" => self.handle_info(stream),
            "/api/frame" => self.handle_frame(stream, &request.query),
            "/api/stream" => self.handle_stream(stream, &request.query),
            _ => unreachable!("non-api routes are handled before acquiring the preview lock"),
        }
    }

    fn handle_info(&mut self, mut stream: TcpStream) -> Result<(), Box<dyn Error>> {
        self.plugin.reload_if_changed()?;
        let timelines = self.plugin.collection()?.timelines();
        let body = info_json(
            self.resolution,
            self.fps,
            &timelines,
            self.plugin.last_error(),
        );
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
                write_response_with_headers(
                    &mut stream,
                    200,
                    "OK",
                    "image/png",
                    &headers,
                    &rendered.body,
                )
            }
            FrameFormat::Rgba => {
                let rendered = self.render_rgba(query)?;
                if self.verbose {
                    log_frame_stats(&rendered.stats);
                }
                let headers = rendered.stats.headers();
                write_response_with_headers(
                    &mut stream,
                    200,
                    "OK",
                    "application/vnd.tellur.rgba",
                    &headers,
                    &rendered.body,
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
        self.plugin.reload_if_changed()?;
        let timelines = self.plugin.collection()?.timelines();
        let Some(info) = select_timeline(&timelines, query.get("timeline")) else {
            return Err("timeline not found".into());
        };
        let seconds = query
            .get("frame")
            .and_then(|v| v.parse::<u64>().ok())
            .map(|frame| frame as f32 / request_fps(query, self.fps.max(1)) as f32)
            .or_else(|| query.get("time").and_then(|v| v.parse::<f32>().ok()))
            .unwrap_or(0.0)
            .clamp(0.0, info.duration.max(0.0));
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
            },
            total_start,
        })
    }
}

struct VideoStreamSetup {
    timeline_id: String,
    duration: f32,
    fps: u32,
    resolution: Resolution,
    gop: u32,
    crf: u8,
    start_seconds: f32,
}

struct VideoFrame {
    image: RasterImage,
    render_time: Duration,
    cache_hits: u64,
    cache_misses: u64,
    bytes_cached: usize,
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
        app.plugin.reload_if_changed()?;
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

        VideoStreamSetup {
            timeline_id: info.id.clone(),
            duration: info.duration,
            fps,
            resolution,
            gop,
            crf,
            start_seconds,
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
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        setup.resolution.width, setup.resolution.height, setup.fps, setup.gop,
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
    let mut stream_out = stream.try_clone()?;
    let client_alive = Arc::new(AtomicBool::new(true));
    let client_alive_for_stdout = Arc::clone(&client_alive);

    let stdout_thread = thread::spawn(move || {
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
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
    });

    let stderr_thread = thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });

    let frame_step = 1.0 / setup.fps as f32;
    let frame_duration = Duration::from_secs_f32(frame_step);
    let remaining = (setup.duration - setup.start_seconds).max(0.0);
    let total_frames = (remaining * setup.fps as f32).ceil() as u64;

    for frame in 0..total_frames {
        if !client_alive.load(Ordering::Relaxed) {
            break;
        }

        let frame_start = Instant::now();
        let seconds = setup.start_seconds + frame as f32 * frame_step;
        let image = {
            let mut app = app
                .lock()
                .map_err(|_| -> Box<dyn Error> { "preview app lock poisoned".into() })?;
            let frame = app.render_video_rgba(&setup.timeline_id, seconds, setup.resolution)?;
            if app.verbose {
                println!(
                    "video timeline={} t={:.3}s size={}x{} fps={} gop={} render={:.2}ms bytes={} cache_delta={}h/{}m cache_size={}",
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
                );
            }
            frame.image
        };

        if stdin.write_all(&image.pixels).is_err() {
            client_alive.store(false, Ordering::Relaxed);
            break;
        }
        sleep_remainder(frame_duration, frame_start.elapsed());
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
    image: RasterImage,
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
        "frame timeline={} t={:.3}s size={}x{} format={} render={:.2}ms encode={:.2}ms total={:.2}ms bytes={} cache_delta={}h/{}m cache_size={}",
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
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
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
) -> String {
    let timelines_json = timelines
        .iter()
        .map(|info| {
            format!(
                "{{\"id\":\"{}\",\"title\":\"{}\",\"duration\":{}}}",
                json_escape(&info.id),
                json_escape(&info.title),
                finite_json_number(info.duration),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let last_error = match last_error {
        Some(e) => format!("\"{}\"", json_escape(e)),
        None => "null".to_owned(),
    };
    format!(
        "{{\"width\":{},\"height\":{},\"fps\":{},\"lastError\":{},\"timelines\":[{}]}}",
        resolution.width, resolution.height, fps, last_error, timelines_json
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

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>tellur live</title>
  <style>
    :root {
      color-scheme: dark;
      --bg: #111318;
      --panel: #1b1f27;
      --line: #343b49;
      --text: #eef1f6;
      --muted: #9aa3b4;
      --accent: #97c9c3;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      min-height: 100vh;
      background: var(--bg);
      color: var(--text);
      font: 14px/1.4 system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      display: grid;
      grid-template-rows: minmax(0, 1fr) auto;
    }
    main {
      min-height: 0;
      display: grid;
      place-items: center;
      padding: 18px;
    }
    .display {
      width: min(100%, calc((100vh - 108px) * var(--aspect)));
      max-height: calc(100vh - 108px);
      aspect-ratio: var(--aspect);
      background: #050608;
      border: 1px solid var(--line);
      display: grid;
      place-items: center;
      overflow: hidden;
    }
    #frame {
      display: block;
    }
    #canvas, #video {
      display: none;
    }
    #frame, #canvas, #video {
      width: 100%;
      height: 100%;
      object-fit: contain;
      image-rendering: auto;
      grid-area: 1 / 1;
    }
    footer {
      border-top: 1px solid var(--line);
      background: var(--panel);
      padding: 12px 14px;
      display: grid;
      grid-template-columns: auto minmax(120px, 1fr) auto auto auto;
      gap: 12px;
      align-items: center;
    }
    button {
      width: 42px;
      height: 34px;
      border: 1px solid var(--line);
      border-radius: 6px;
      background: #242a35;
      color: var(--text);
      font: 700 16px/1 system-ui, sans-serif;
      cursor: pointer;
    }
    button:hover { border-color: var(--accent); }
    input[type="range"] {
      width: 100%;
      accent-color: var(--accent);
    }
    select {
      height: 34px;
      border: 1px solid var(--line);
      border-radius: 6px;
      background: #242a35;
      color: var(--text);
      padding: 0 8px;
      font: 13px/1 system-ui, sans-serif;
    }
    .control {
      display: inline-flex;
      align-items: center;
      gap: 6px;
      color: var(--muted);
      font-size: 12px;
      white-space: nowrap;
    }
    .readout {
      min-width: 190px;
      text-align: right;
      color: var(--muted);
      font-variant-numeric: tabular-nums;
      white-space: nowrap;
    }
    .readout strong { color: var(--text); font-weight: 600; }
    .error {
      position: fixed;
      left: 12px;
      top: 12px;
      max-width: min(720px, calc(100vw - 24px));
      padding: 8px 10px;
      border: 1px solid #8f4f4f;
      background: #2a1719;
      color: #ffd6d6;
      display: none;
    }
    @media (max-width: 720px) {
      footer {
        grid-template-columns: auto minmax(120px, 1fr) auto;
      }
      .control {
        justify-self: start;
      }
    }
  </style>
</head>
<body>
  <main>
    <div class="display" id="display"><video id="video" muted playsinline preload="auto"></video><canvas id="canvas"></canvas><img id="frame" alt=""></div>
  </main>
  <footer>
    <button id="play" type="button" aria-label="Play">></button>
    <input id="seek" type="range" min="0" value="0" step="0.001">
    <div class="readout"><strong id="seconds">0.000s</strong> / <span id="frameNo">0</span>f</div>
    <label class="control">Size
      <select id="scale">
        <option value="1">100%</option>
        <option value="0.75">75%</option>
        <option value="0.5">50%</option>
        <option value="0.25">25%</option>
      </select>
    </label>
    <label class="control">FPS
      <select id="fps">
        <option value="60">60</option>
        <option value="30" selected>30</option>
        <option value="24">24</option>
        <option value="15">15</option>
        <option value="12">12</option>
      </select>
    </label>
  </footer>
  <div class="error" id="error"></div>
  <script>
    const img = document.getElementById("frame");
    const video = document.getElementById("video");
    const canvas = document.getElementById("canvas");
    const canvasCtx = canvas.getContext("2d");
    const display = document.getElementById("display");
    const play = document.getElementById("play");
    const seek = document.getElementById("seek");
    const scaleSelect = document.getElementById("scale");
    const fpsSelect = document.getElementById("fps");
    const secondsOut = document.getElementById("seconds");
    const frameOut = document.getElementById("frameNo");
    const error = document.getElementById("error");

    let info = null;
    let timeline = null;
    let playing = false;
    let seconds = 0;
    let startedAt = 0;
    let baseSeconds = 0;
    let displayToken = 0;
    let pngToken = 0;
    let preloadToken = 0;
    let pendingPng = false;
    let queuedPng = false;
    let preloadTimer = null;
    let preloadedVideoKey = "";
    let controlsInitialized = false;

    async function loadInfo() {
      const response = await fetch("/api/info", { cache: "no-store" });
      info = await response.json();
      timeline = info.timelines[0];
      if (!controlsInitialized) {
        const fpsValue = String(Math.max(info.fps || 30, 1));
        if ([...fpsSelect.options].some((option) => option.value === fpsValue)) {
          fpsSelect.value = fpsValue;
        }
        controlsInitialized = true;
      }
      const aspect = info.width / info.height;
      display.style.setProperty("--aspect", String(aspect));
      seek.max = timeline ? String(timeline.duration) : "0";
      seek.step = String(1 / selectedFps());
      showError(info.lastError);
    }

    function showError(message) {
      error.style.display = message ? "block" : "none";
      error.textContent = message || "";
    }

    function updateReadout() {
      const fps = selectedFps();
      secondsOut.textContent = `${seconds.toFixed(3)}s`;
      frameOut.textContent = String(Math.round(seconds * fps));
      seek.value = String(seconds);
    }

    function selectedFps() {
      return Math.max(Number(fpsSelect.value) || (info ? info.fps : 30) || 30, 1);
    }

    function selectedResolution() {
      if (!info) return { width: 1, height: 1 };
      const scale = Math.max(Number(scaleSelect.value) || 1, 0.01);
      return {
        width: Math.max(1, Math.round(info.width * scale)),
        height: Math.max(1, Math.round(info.height * scale))
      };
    }

    function applyRenderParams(params) {
      const target = selectedResolution();
      params.set("width", String(target.width));
      params.set("height", String(target.height));
      params.set("fps", String(selectedFps()));
      return target;
    }

    function videoGop() {
      return Math.max(1, Math.floor(selectedFps() / 4));
    }

    function videoKey(frameSeconds = seconds) {
      if (!timeline || !info) return "";
      const target = selectedResolution();
      return [
        timeline.id,
        frameSeconds.toFixed(4),
        `${target.width}x${target.height}`,
        String(selectedFps()),
        String(videoGop()),
        "23"
      ].join("|");
    }

    function videoUrl(frameSeconds, token) {
      const fps = selectedFps();
      const params = new URLSearchParams({
        timeline: timeline.id,
        time: frameSeconds.toFixed(4),
        fps: String(fps),
        gop: String(videoGop()),
        crf: "23",
        _: String(token)
      });
      applyRenderParams(params);
      return `/api/video.mp4?${params}`;
    }

    function frameParams(format, token, frameSeconds = seconds) {
      return new URLSearchParams({
        timeline: timeline.id,
        time: frameSeconds.toFixed(4),
        format,
        _: String(token)
      });
    }

    function showCanvas() {
      canvas.style.display = "block";
      img.style.display = "none";
      video.style.display = "none";
    }

    function showImage() {
      img.style.display = "block";
      canvas.style.display = "none";
      video.style.display = "none";
    }

    function showVideo() {
      video.style.display = "block";
      img.style.display = "none";
      canvas.style.display = "none";
    }

    function stopVideo() {
      clearVideoPreload();
    }

    function clearVideoPreload() {
      clearTimeout(preloadTimer);
      if (!video.src && !preloadedVideoKey) return;
      video.pause();
      video.removeAttribute("src");
      video.load();
      preloadedVideoKey = "";
      preloadToken += 1;
    }

    function requestPngFrame(force = false) {
      if (!timeline) return;
      if (pendingPng) {
        queuedPng = true;
        if (force) displayToken += 1;
        clearTimeout(preloadTimer);
        return;
      }
      pendingPng = true;
      queuedPng = false;
      clearTimeout(preloadTimer);
      const id = ++displayToken;
      const pngId = ++pngToken;
      const params = frameParams("png", id);
      applyRenderParams(params);

      const finish = () => {
        if (pngId === pngToken) pendingPng = false;
        if (queuedPng && !playing) {
          queuedPng = false;
          requestPngFrame(true);
        } else if (!playing) {
          scheduleVideoPreload();
        }
      };

      img.onload = () => {
        if (id === displayToken) {
          showImage();
        }
        finish();
      };
      img.onerror = () => {
        if (id === displayToken) showError("frame request failed");
        finish();
      };
      img.src = `/api/frame?${params}`;
    }

    function scheduleVideoPreload(delay = 260) {
      clearTimeout(preloadTimer);
      if (!timeline || !info || playing) return;
      preloadTimer = setTimeout(preloadVideo, delay);
    }

    function preloadVideo() {
      if (!timeline || !info || playing) return;
      const key = videoKey(seconds);
      if (key && key === preloadedVideoKey && video.src) return;
      const token = ++preloadToken;
      preloadedVideoKey = key;
      video.pause();
      video.onloadeddata = null;
      video.onerror = null;
      video.onended = null;
      video.src = videoUrl(seconds, `preload-${token}`);
      video.load();
    }

    function startVideoPlayback() {
      if (!timeline || !info) return;
      const token = ++displayToken;
      const startSeconds = seconds;
      baseSeconds = startSeconds;
      startedAt = performance.now();
      clearTimeout(preloadTimer);
      const key = videoKey(startSeconds);

      video.onloadeddata = () => { if (token === displayToken) showVideo(); };
      video.onerror = () => {
        if (token === displayToken) showError("video stream failed");
      };
      video.onended = () => {
        if (token !== displayToken) return;
        playing = false;
        play.textContent = ">";
        seconds = Math.min(startSeconds + video.currentTime, timeline.duration);
        updateReadout();
        requestPngFrame(true);
      };
      if (key !== preloadedVideoKey || !video.src || video.error) {
        preloadedVideoKey = key;
        video.src = videoUrl(startSeconds, token);
      }
      if (video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA) {
        showVideo();
      }
      video.play().catch((e) => {
        if (token === displayToken) showError(String(e));
      });

      function syncReadout() {
        if (!playing || token !== displayToken) return;
        seconds = Math.min(startSeconds + video.currentTime, timeline.duration);
        updateReadout();
        requestAnimationFrame(syncReadout);
      }
      requestAnimationFrame(syncReadout);
    }

    play.addEventListener("click", () => {
      playing = !playing;
      play.textContent = playing ? "||" : ">";
      if (playing) {
        baseSeconds = seconds;
        startedAt = performance.now();
        startVideoPlayback();
      } else {
        seconds = Math.min(baseSeconds + video.currentTime, timeline ? timeline.duration : seconds);
        stopVideo();
        requestPngFrame(true);
      }
    });

    seek.addEventListener("input", () => {
      clearVideoPreload();
      if (playing) {
        playing = false;
        play.textContent = ">";
      }
      seconds = Number(seek.value);
      baseSeconds = seconds;
      startedAt = performance.now();
      updateReadout();
      requestPngFrame(true);
    });

    function applyPreviewSettings() {
      if (!info) return;
      seek.step = String(1 / selectedFps());
      updateReadout();
      clearTimeout(preloadTimer);
      if (playing) {
        stopVideo();
        startVideoPlayback();
      } else {
        clearVideoPreload();
        requestPngFrame(true);
      }
    }

    scaleSelect.addEventListener("change", applyPreviewSettings);
    fpsSelect.addEventListener("change", applyPreviewSettings);

    loadInfo()
      .then(() => {
        updateReadout();
        requestPngFrame(true);
        setInterval(() => { if (!playing) loadInfo(); }, 1000);
      })
      .catch((e) => showError(String(e)));
  </script>
</body>
</html>
"#;
