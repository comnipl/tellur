use std::net::SocketAddr;

use tellur_core::cache_budget::{configured_cache_ram_bytes, configured_vram_bytes};
use tellur_core::render_context::GpuPreference;
use tellur_renderer::{host_cpu_summary, probe_adapter_info};

const LABEL_WIDTH: usize = 7;

pub fn print_startup_banner(listen_addr: SocketAddr, gpu_preference: GpuPreference) {
    let listen = format!("http://{listen_addr}");
    let cpu = host_cpu_summary();
    let gpu = gpu_summary(gpu_preference);
    let ram = format!("{}  cache budget", format_budget_bytes(configured_cache_ram_bytes()));
    let vram = format!("{}  GPU budget", format_budget_bytes(configured_vram_bytes()));

    eprintln!("tellur live");
    eprintln!("{}", labeled_line("listen", &listen));
    eprintln!("{}", labeled_line("cpu", &cpu));
    eprintln!("{}", labeled_line("gpu", &gpu));
    eprintln!("{}", labeled_line("ram", &ram));
    eprintln!("{}", labeled_line("vram", &vram));
}

fn gpu_summary(gpu_preference: GpuPreference) -> String {
    if !gpu_preference.prefers_gpu() {
        return "disabled".to_string();
    }
    probe_adapter_info()
        .map(|info| format!("{} ({})", info.name, info.backend))
        .unwrap_or_else(|| "unavailable".to_string())
}

fn labeled_line(label: &str, value: &str) -> String {
    format!("  {label:<LABEL_WIDTH$}  {value}")
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
    use super::{format_budget_bytes, gpu_summary, labeled_line};
    use tellur_core::render_context::GpuPreference;

    #[test]
    fn labeled_line_aligns_values() {
        assert_eq!(labeled_line("listen", "http://127.0.0.1:4317"), "  listen   http://127.0.0.1:4317");
    }

    #[test]
    fn format_budget_bytes_uses_gib() {
        assert_eq!(format_budget_bytes(1024 * 1024 * 1024), "1.00 GiB");
    }

    #[test]
    fn gpu_summary_disabled_skips_probe() {
        assert_eq!(gpu_summary(GpuPreference::Disabled), "disabled");
    }
}
