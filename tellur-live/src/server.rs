use std::collections::HashMap;
use std::error::Error;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use tellur_core::raster::Resolution;
use tellur_core::time::TimelineTime;
use tellur_renderer::CachingRenderContext;

use crate::plugin::{HotReloadPlugin, TimelineInfo};

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub plugin_path: PathBuf,
    pub bind: String,
    pub resolution: Resolution,
    pub fps: u32,
}

pub fn serve(options: ServerOptions) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind(&options.bind)?;
    let local_addr = listener.local_addr()?;
    eprintln!("tellur live listening on http://{local_addr}");
    eprintln!("plugin: {}", options.plugin_path.display());

    let mut app = PreviewApp {
        plugin: HotReloadPlugin::new(options.plugin_path),
        ctx: CachingRenderContext::new(),
        resolution: options.resolution,
        fps: options.fps,
    };
    app.plugin.reload_if_changed()?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(e) = app.handle(stream) {
                    eprintln!("request failed: {e}");
                }
            }
            Err(e) => eprintln!("accept failed: {e}"),
        }
    }
    Ok(())
}

struct PreviewApp {
    plugin: HotReloadPlugin,
    ctx: CachingRenderContext,
    resolution: Resolution,
    fps: u32,
}

impl PreviewApp {
    fn handle(&mut self, mut stream: TcpStream) -> Result<(), Box<dyn Error>> {
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

        match request.path.as_str() {
            "/" | "/index.html" => write_response(
                &mut stream,
                200,
                "OK",
                "text/html; charset=utf-8",
                INDEX_HTML.as_bytes(),
            ),
            "/api/info" => self.handle_info(stream),
            "/api/frame" => self.handle_frame(stream, &request.query),
            "/api/stream" => self.handle_stream(stream, &request.query),
            _ => write_response(
                &mut stream,
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"not found",
            ),
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
        let png = self.render_png(query)?;
        write_response(&mut stream, 200, "OK", "image/png", &png)
    }

    fn handle_stream(
        &mut self,
        mut stream: TcpStream,
        query: &HashMap<String, String>,
    ) -> Result<(), Box<dyn Error>> {
        let fps = query
            .get("fps")
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|fps| *fps > 0)
            .unwrap_or(self.fps.max(1));
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
        loop {
            let mut q = HashMap::new();
            q.insert("time".to_owned(), seconds.to_string());
            if let Some(id) = &timeline_id {
                q.insert("timeline".to_owned(), id.clone());
            }
            let png = self.render_png(&q)?;
            write!(
                stream,
                "--tellur-frame\r\n\
                 Content-Type: image/png\r\n\
                 Content-Length: {}\r\n\r\n",
                png.len()
            )?;
            stream.write_all(&png)?;
            stream.write_all(b"\r\n")?;
            stream.flush()?;
            seconds += frame_step;
            thread::sleep(Duration::from_secs_f32(frame_step));
        }
    }

    fn render_png(&mut self, query: &HashMap<String, String>) -> Result<Vec<u8>, Box<dyn Error>> {
        self.plugin.reload_if_changed()?;
        let timelines = self.plugin.collection()?.timelines();
        let Some(info) = select_timeline(&timelines, query.get("timeline")) else {
            return Err("timeline not found".into());
        };
        let seconds = query
            .get("frame")
            .and_then(|v| v.parse::<u64>().ok())
            .map(|frame| frame as f32 / self.fps.max(1) as f32)
            .or_else(|| query.get("time").and_then(|v| v.parse::<f32>().ok()))
            .unwrap_or(0.0)
            .clamp(0.0, info.duration.max(0.0));

        let image = self
            .plugin
            .collection()?
            .build(
                &info.id,
                TimelineTime::new(seconds),
                self.resolution,
                &mut self.ctx,
            )
            .ok_or("timeline did not produce a frame")?;
        let mut png = Vec::new();
        image.export_png(&mut png)?;
        Ok(png)
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
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    Ok(())
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
      width: 100%;
      height: 100%;
      object-fit: contain;
      image-rendering: auto;
    }
    footer {
      border-top: 1px solid var(--line);
      background: var(--panel);
      padding: 12px 14px;
      display: grid;
      grid-template-columns: auto minmax(120px, 1fr) auto;
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
  </style>
</head>
<body>
  <main>
    <div class="display" id="display"><img id="frame" alt=""></div>
  </main>
  <footer>
    <button id="play" type="button" aria-label="Play">></button>
    <input id="seek" type="range" min="0" value="0" step="0.001">
    <div class="readout"><strong id="seconds">0.000s</strong> / <span id="frameNo">0</span>f</div>
  </footer>
  <div class="error" id="error"></div>
  <script>
    const img = document.getElementById("frame");
    const display = document.getElementById("display");
    const play = document.getElementById("play");
    const seek = document.getElementById("seek");
    const secondsOut = document.getElementById("seconds");
    const frameOut = document.getElementById("frameNo");
    const error = document.getElementById("error");

    let info = null;
    let timeline = null;
    let playing = false;
    let seconds = 0;
    let startedAt = 0;
    let baseSeconds = 0;
    let requestId = 0;
    let pending = false;

    async function loadInfo() {
      const response = await fetch("/api/info", { cache: "no-store" });
      info = await response.json();
      timeline = info.timelines[0];
      const aspect = info.width / info.height;
      display.style.setProperty("--aspect", String(aspect));
      seek.max = timeline ? String(timeline.duration) : "0";
      seek.step = String(1 / Math.max(info.fps, 1));
      showError(info.lastError);
    }

    function showError(message) {
      error.style.display = message ? "block" : "none";
      error.textContent = message || "";
    }

    function updateReadout() {
      const fps = info ? Math.max(info.fps, 1) : 1;
      secondsOut.textContent = `${seconds.toFixed(3)}s`;
      frameOut.textContent = String(Math.round(seconds * fps));
      seek.value = String(seconds);
    }

    function requestFrame(force = false) {
      if (!timeline || (pending && !force)) return;
      pending = true;
      const id = ++requestId;
      const params = new URLSearchParams({
        timeline: timeline.id,
        time: seconds.toFixed(4),
        _: String(id)
      });
      img.onload = () => { if (id === requestId) pending = false; };
      img.onerror = () => {
        if (id === requestId) pending = false;
        showError("frame request failed");
      };
      img.src = `/api/frame?${params}`;
    }

    function tick(now) {
      if (!playing || !timeline) return;
      seconds = baseSeconds + (now - startedAt) / 1000;
      if (seconds >= timeline.duration) {
        seconds = timeline.duration;
        playing = false;
        play.textContent = ">";
      }
      updateReadout();
      requestFrame();
      if (playing) requestAnimationFrame(tick);
    }

    play.addEventListener("click", () => {
      playing = !playing;
      play.textContent = playing ? "||" : ">";
      if (playing) {
        startedAt = performance.now();
        baseSeconds = seconds;
        requestAnimationFrame(tick);
      }
    });

    seek.addEventListener("input", () => {
      seconds = Number(seek.value);
      baseSeconds = seconds;
      startedAt = performance.now();
      updateReadout();
      requestFrame(true);
    });

    loadInfo()
      .then(() => {
        updateReadout();
        requestFrame(true);
        setInterval(loadInfo, 1000);
      })
      .catch((e) => showError(String(e)));
  </script>
</body>
</html>
"#;
