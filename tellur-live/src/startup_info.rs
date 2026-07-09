use std::collections::HashSet;
use std::env;
use std::io::{IsTerminal, stderr};
use std::net::{IpAddr, SocketAddr};
use std::time::Instant;

use tellur_core::cache_budget::{configured_cache_ram_bytes, configured_vram_bytes};
use tellur_core::render_context::GpuPreference;
use tellur_renderer::{host_cpu_summary, host_memory_total_bytes, probe_adapter_info};

const LABEL_WIDTH: usize = 9;

pub fn print_startup_banner(
    listen_addr: SocketAddr,
    gpu_preference: GpuPreference,
    started_at: Instant,
) {
    let style = Style::new();
    let lines = build_banner_lines(listen_addr, gpu_preference, started_at);
    for line in lines {
        eprintln!("{}", style.render_line(line));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BannerLine {
    Header { version: String, ready_ms: u128 },
    Info { label: &'static str, value: String, warn: bool, link: Option<String> },
    Blank,
}

fn build_banner_lines(
    listen_addr: SocketAddr,
    gpu_preference: GpuPreference,
    started_at: Instant,
) -> Vec<BannerLine> {
    let (gpu, gpu_warn) = gpu_summary(gpu_preference);
    let ram_budget = format_budget_bytes(configured_cache_ram_bytes());
    let ram_total = format_budget_bytes(host_memory_total_bytes() as usize);
    let ram = format!("{ram_budget} cache budget (of {ram_total})");
    let vram = format!(
        "{} GPU budget",
        format_budget_bytes(configured_vram_bytes())
    );

    let local_url = local_listen_url(listen_addr);
    let mut lines = vec![
        BannerLine::Header {
            version: version_label(),
            ready_ms: started_at.elapsed().as_millis(),
        },
        BannerLine::Blank,
        BannerLine::Info {
            label: "Local:",
            value: local_url.clone(),
            warn: false,
            link: Some(local_url),
        },
    ];

    for url in network_urls(listen_addr) {
        lines.push(BannerLine::Info {
            label: "Network:",
            value: url.clone(),
            warn: false,
            link: Some(url),
        });
    }

    lines.extend([
        BannerLine::Info {
            label: "CPU:",
            value: host_cpu_summary(),
            warn: false,
            link: None,
        },
        BannerLine::Info {
            label: "GPU:",
            value: gpu,
            warn: gpu_warn,
            link: None,
        },
        BannerLine::Info {
            label: "RAM:",
            value: ram,
            warn: false,
            link: None,
        },
        BannerLine::Info {
            label: "VRAM:",
            value: vram,
            warn: false,
            link: None,
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
            BannerLine::Info {
                label,
                value,
                warn,
                link,
            } => self.render_info(label, &value, warn, link.as_deref()),
        }
    }

    fn render_header(&self, version: &str, ready_ms: u128) -> String {
        let title = self.bold(&self.cyan("tellur live"));
        let version = self.dim(version);
        let ready = self.dim(&format!("ready in {ready_ms} ms"));
        format!("  {title} {version}  {ready}")
    }

    fn render_info(&self, label: &str, value: &str, warn: bool, link: Option<&str>) -> String {
        let marker = self.cyan("➜");
        let label = self.dim(label);
        let value = if warn {
            self.yellow(value)
        } else if let Some(url) = link {
            self.hyperlink(url, &self.bold(value))
        } else {
            value.to_string()
        };
        format!("  {marker}  {label:<LABEL_WIDTH$}  {value}")
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

fn local_listen_url(listen_addr: SocketAddr) -> String {
    let port = listen_addr.port();
    if listen_addr.ip().is_unspecified() {
        format!("http://127.0.0.1:{port}/")
    } else {
        format_http_url(listen_addr.ip(), port)
    }
}

fn network_urls(listen_addr: SocketAddr) -> Vec<String> {
    if !listen_addr.ip().is_unspecified() {
        return Vec::new();
    }

    let port = listen_addr.port();
    let mut seen = HashSet::new();
    local_ip_address::list_afinet_netifas()
        .unwrap_or_default()
        .into_iter()
        .map(|(_, ip)| ip)
        .filter(|ip| !ip.is_loopback())
        .filter_map(|ip| {
            if seen.insert(ip) {
                Some(format_http_url(ip, port))
            } else {
                None
            }
        })
        .collect()
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
    format!("  ➜  {label:<LABEL_WIDTH$}  {value}")
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
    use std::time::Instant;

    use super::{
        banner_line, build_banner_lines, format_budget_bytes, format_http_url, gpu_summary,
        local_listen_url, network_urls, version_label, BannerLine,
    };
    use tellur_core::render_context::GpuPreference;

    #[test]
    fn banner_line_aligns_values() {
        assert_eq!(
            banner_line("Local:", "http://127.0.0.1:4317/"),
            "  ➜  Local:     http://127.0.0.1:4317/"
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
    fn local_listen_url_uses_loopback_for_unspecified_bind() {
        let addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 4317).into();
        assert_eq!(local_listen_url(addr), "http://127.0.0.1:4317/");
    }

    #[test]
    fn local_listen_url_preserves_specific_bind() {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4317).into();
        assert_eq!(local_listen_url(addr), "http://127.0.0.1:4317/");
    }

    #[test]
    fn format_http_url_wraps_ipv6() {
        use std::net::{IpAddr, Ipv6Addr};
        let ip = IpAddr::V6(Ipv6Addr::LOCALHOST);
        assert_eq!(format_http_url(ip, 4317), "http://[::1]:4317/");
    }

    #[test]
    fn network_urls_empty_for_specific_bind() {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4317).into();
        assert!(network_urls(addr).is_empty());
    }

    #[test]
    fn build_banner_includes_ram_total_suffix() {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4317).into();
        let lines = build_banner_lines(addr, GpuPreference::Disabled, Instant::now());
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
