use std::env;
use std::io::{IsTerminal, stderr};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Instant;

use tellur_core::cache_budget::{configured_cache_ram_bytes, configured_vram_bytes};
use tellur_core::render_context::GpuPreference;
use tellur_renderer::{host_cpu_summary, host_memory_total_bytes, probe_adapter_info};

use crate::build_watch::AutoBuildOptions;

const INFO_INDENT: &str = "    ";
const LABEL_WIDTH: usize = 8;

pub struct StartupBannerInputs<'a> {
    pub listen_addr: SocketAddr,
    pub plugin_path: &'a Path,
    pub gpu_preference: GpuPreference,
    pub auto_build: Option<&'a AutoBuildOptions>,
    pub started_at: Instant,
}

pub fn print_startup_banner(inputs: StartupBannerInputs<'_>) {
    let style = Style::new();
    let lines = build_banner_lines(inputs);
    for line in lines {
        eprintln!("{}", style.render_line(line));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BannerLine {
    Header { version: String, ready_ms: u128 },
    Info {
        label: &'static str,
        value: String,
        warn: bool,
    },
    OpenBrowser { url: String },
    Blank,
}

fn build_banner_lines(inputs: StartupBannerInputs<'_>) -> Vec<BannerLine> {
    let StartupBannerInputs {
        listen_addr,
        plugin_path,
        gpu_preference,
        auto_build,
        started_at,
    } = inputs;

    let (gpu, gpu_warn) = gpu_summary(gpu_preference);
    let ram_budget = format_budget_bytes(configured_cache_ram_bytes());
    let ram_total = format_budget_bytes(host_memory_total_bytes() as usize);
    let ram = format!("{ram_budget} cache budget (of {ram_total})");
    let vram = format!(
        "{} GPU budget",
        format_budget_bytes(configured_vram_bytes())
    );

    let mut lines = vec![
        BannerLine::Blank,
        BannerLine::Header {
            version: version_label(),
            ready_ms: started_at.elapsed().as_millis(),
        },
        BannerLine::Info {
            label: "Plugin:",
            value: plugin_path.display().to_string(),
            warn: false,
        },
    ];

    if let Some(auto_build) = auto_build {
        lines.push(BannerLine::Info {
            label: "Watch:",
            value: format_watch_paths(&auto_build.watch_paths),
            warn: false,
        });
    }

    lines.push(BannerLine::Info {
        label: "Host:",
        value: host_display_value(listen_addr),
        warn: false,
    });

    lines.extend([
        BannerLine::Info {
            label: "CPU:",
            value: host_cpu_summary(),
            warn: false,
        },
        BannerLine::Info {
            label: "GPU:",
            value: gpu,
            warn: gpu_warn,
        },
        BannerLine::Info {
            label: "RAM:",
            value: ram,
            warn: false,
        },
        BannerLine::Info {
            label: "VRAM:",
            value: vram,
            warn: false,
        },
        BannerLine::Blank,
        BannerLine::OpenBrowser {
            url: browser_open_url(listen_addr),
        },
    ]);

    lines
}

struct Style {
    enabled: bool,
}

impl Style {
    fn new() -> Self {
        let enabled = stderr().is_terminal()
            && env::var_os("NO_COLOR").is_none()
            && env::var("TERM").ok().as_deref() != Some("dumb");
        Self { enabled }
    }

    fn render_line(&self, line: BannerLine) -> String {
        match line {
            BannerLine::Header { version, ready_ms } => self.render_header(&version, ready_ms),
            BannerLine::Blank => String::new(),
            BannerLine::Info { label, value, warn } => self.render_info(label, &value, warn),
            BannerLine::OpenBrowser { url } => self.render_open_browser(&url),
        }
    }

    fn render_header(&self, version: &str, ready_ms: u128) -> String {
        let title = self.bold(&self.cyan("Tellur Live"));
        let version = self.dim(version);
        let ready = self.dim(&format!("ready in {ready_ms} ms"));
        format!("  {title} {version}  {ready}")
    }

    fn render_info(&self, label: &str, value: &str, warn: bool) -> String {
        let label = self.dim(&format!("{label:>LABEL_WIDTH$}"));
        let value = if warn {
            self.yellow(value)
        } else {
            value.to_string()
        };
        format!("{INFO_INDENT}{label}  {value}")
    }

    fn render_open_browser(&self, url: &str) -> String {
        let arrow = self.cyan("→");
        let prefix = self.dim("Open ");
        // Keep OSC 8 free of nested SGR codes so terminals can recognize the link.
        let linked = self.hyperlink(url, url);
        let suffix = self.dim(" in your browser");
        format!("  {arrow} {prefix}{linked}{suffix}")
    }

    fn bold(&self, text: &str) -> String {
        if self.enabled {
            format!("\x1b[1m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn dim(&self, text: &str) -> String {
        if self.enabled {
            format!("\x1b[2m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn cyan(&self, text: &str) -> String {
        if self.enabled {
            format!("\x1b[36m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn yellow(&self, text: &str) -> String {
        if self.enabled {
            format!("\x1b[33m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn hyperlink(&self, url: &str, text: &str) -> String {
        if self.enabled {
            format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
        } else {
            text.to_string()
        }
    }
}

fn version_label() -> String {
    let version = env!("CARGO_PKG_VERSION");
    #[cfg(debug_assertions)]
    {
        format!("v{version} (debug)")
    }
    #[cfg(not(debug_assertions))]
    {
        format!("v{version}")
    }
}

fn host_display_value(listen_addr: SocketAddr) -> String {
    if listen_addr.ip().is_unspecified() {
        format_bind_host(listen_addr)
    } else {
        local_listen_url(listen_addr)
    }
}

fn browser_open_url(listen_addr: SocketAddr) -> String {
    if listen_addr.ip().is_unspecified() {
        format!("http://127.0.0.1:{}/", listen_addr.port())
    } else {
        local_listen_url(listen_addr)
    }
}

fn format_bind_host(listen_addr: SocketAddr) -> String {
    let port = listen_addr.port();
    match listen_addr.ip() {
        IpAddr::V4(_) => format!("0.0.0.0:{port}"),
        IpAddr::V6(_) => format!("[::]:{port}"),
    }
}

fn format_watch_paths(paths: &[PathBuf]) -> String {
    let paths = if paths.is_empty() {
        vec![env::current_dir().unwrap_or_else(|_| PathBuf::from("."))]
    } else {
        paths.to_vec()
    };
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn local_listen_url(listen_addr: SocketAddr) -> String {
    format_http_url(listen_addr.ip(), listen_addr.port())
}

fn format_http_url(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(v4) => format!("http://{v4}:{port}/"),
        IpAddr::V6(v6) => format!("http://[{v6}]:{port}/"),
    }
}

fn gpu_summary(gpu_preference: GpuPreference) -> (String, bool) {
    if !gpu_preference.prefers_gpu() {
        return ("disabled (rendering on CPU)".to_string(), true);
    }
    probe_adapter_info()
        .map(|info| (format!("{} ({})", info.name, info.backend), false))
        .unwrap_or_else(|| ("unavailable (rendering on CPU)".to_string(), true))
}

#[cfg(test)]
fn banner_line(label: &str, value: &str) -> String {
    format!("{INFO_INDENT}{label:>LABEL_WIDTH$}  {value}")
}

#[cfg(test)]
fn open_browser_line(url: &str) -> String {
    format!("  → Open {url} in your browser")
}

fn format_budget_bytes(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bf = bytes as f64;
    if bf >= GIB {
        format!("{:.2} GiB", bf / GIB)
    } else if bf >= MIB {
        format!("{:.2} MiB", bf / MIB)
    } else if bf >= KIB {
        format!("{:.2} KiB", bf / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use super::{
        banner_line, browser_open_url, build_banner_lines, format_bind_host, format_budget_bytes,
        format_http_url, format_watch_paths, gpu_summary, local_listen_url, open_browser_line,
        version_label, BannerLine, StartupBannerInputs,
    };
    use crate::build_watch::AutoBuildOptions;
    use tellur_core::render_context::GpuPreference;

    const PLUGIN: &str = "target/release/examples/libdemo_timeline_plugin.so";

    fn banner_inputs<'a>(
        listen_addr: SocketAddrV4,
        auto_build: Option<&'a AutoBuildOptions>,
    ) -> StartupBannerInputs<'a> {
        StartupBannerInputs {
            listen_addr: listen_addr.into(),
            plugin_path: Path::new(PLUGIN),
            gpu_preference: GpuPreference::Disabled,
            auto_build,
            started_at: Instant::now(),
        }
    }

    #[test]
    fn banner_line_aligns_values() {
        assert_eq!(
            banner_line("Host:", "http://127.0.0.1:4317/"),
            "       Host:  http://127.0.0.1:4317/"
        );
        assert_eq!(
            banner_line("Plugin:", "target/libfoo.so"),
            "     Plugin:  target/libfoo.so"
        );
    }

    #[test]
    fn open_browser_line_formats_hint() {
        assert_eq!(
            open_browser_line("http://127.0.0.1:4317/"),
            "  → Open http://127.0.0.1:4317/ in your browser"
        );
    }

    #[test]
    fn format_budget_bytes_uses_gib() {
        assert_eq!(format_budget_bytes(1024 * 1024 * 1024), "1.00 GiB");
    }

    #[test]
    fn gpu_summary_disabled_skips_probe() {
        assert_eq!(
            gpu_summary(GpuPreference::Disabled),
            ("disabled (rendering on CPU)".to_string(), true)
        );
    }

    #[test]
    fn local_listen_url_preserves_specific_bind() {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4317).into();
        assert_eq!(local_listen_url(addr), "http://127.0.0.1:4317/");
    }

    #[test]
    fn browser_open_url_uses_loopback_for_unspecified_bind() {
        let addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 4318).into();
        assert_eq!(browser_open_url(addr), "http://127.0.0.1:4318/");
    }

    #[test]
    fn format_http_url_wraps_ipv6() {
        use std::net::{IpAddr, Ipv6Addr};
        let ip = IpAddr::V6(Ipv6Addr::LOCALHOST);
        assert_eq!(format_http_url(ip, 4317), "http://[::1]:4317/");
    }

    #[test]
    fn format_bind_host_for_unspecified_ipv4() {
        let addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 4317).into();
        assert_eq!(format_bind_host(addr), "0.0.0.0:4317");
    }

    #[test]
    fn format_watch_paths_joins_multiple_paths() {
        let summary = format_watch_paths(&[
            PathBuf::from("src"),
            PathBuf::from("examples/demo.rs"),
        ]);
        assert_eq!(summary, "src, examples/demo.rs");
    }

    #[test]
    fn build_banner_starts_with_blank_line_and_plugin() {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4317);
        let lines = build_banner_lines(banner_inputs(addr, None));
        assert!(matches!(lines.first(), Some(BannerLine::Blank)));
        assert!(lines.iter().any(|line| matches!(
            line,
            BannerLine::Info { label: "Plugin:", .. }
        )));
    }

    #[test]
    fn build_banner_uses_host_label_for_specific_bind() {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4317);
        let lines = build_banner_lines(banner_inputs(addr, None));
        assert!(lines.iter().any(|line| matches!(
            line,
            BannerLine::Info {
                label: "Host:",
                value,
                ..
            } if value == "http://127.0.0.1:4317/"
        )));
    }

    #[test]
    fn build_banner_uses_host_for_unspecified_bind() {
        let addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 4317);
        let lines = build_banner_lines(banner_inputs(addr, None));
        assert!(lines.iter().any(|line| matches!(
            line,
            BannerLine::Info {
                label: "Host:",
                value,
                ..
            } if value == "0.0.0.0:4317"
        )));
        assert!(!lines.iter().any(|line| matches!(
            line,
            BannerLine::Info { label: "Local:", .. }
        )));
    }

    #[test]
    fn build_banner_has_blank_line_before_open_hint() {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4317);
        let lines = build_banner_lines(banner_inputs(addr, None));
        assert!(matches!(
            lines[lines.len() - 2],
            BannerLine::Blank
        ));
        assert!(matches!(
            lines.last(),
            Some(BannerLine::OpenBrowser { .. })
        ));
    }

    #[test]
    fn build_banner_ends_with_open_browser_hint() {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4317);
        let lines = build_banner_lines(banner_inputs(addr, None));
        assert!(matches!(
            lines.last(),
            Some(BannerLine::OpenBrowser { url }) if url == "http://127.0.0.1:4317/"
        ));
    }

    #[test]
    fn build_banner_includes_watch_paths_when_auto_build_enabled() {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4317);
        let auto_build = AutoBuildOptions {
            package: Some("demo".to_owned()),
            example: None,
            release: true,
            manifest_path: None,
            watch_paths: vec![PathBuf::from("src")],
            poll_interval: Duration::from_millis(250),
        };
        let lines = build_banner_lines(banner_inputs(addr, Some(&auto_build)));
        assert!(lines.iter().any(|line| matches!(
            line,
            BannerLine::Info {
                label: "Watch:",
                value,
                ..
            } if value == "src"
        )));
    }

    #[test]
    fn build_banner_includes_ram_total_suffix() {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4317);
        let lines = build_banner_lines(banner_inputs(addr, None));
        let ram = lines
            .iter()
            .find_map(|line| match line {
                BannerLine::Info { label: "RAM:", value, .. } => Some(value.clone()),
                _ => None,
            })
            .expect("ram line");
        assert!(ram.contains("cache budget (of "));
    }

    #[test]
    fn version_label_is_non_empty() {
        assert!(!version_label().is_empty());
    }
}
