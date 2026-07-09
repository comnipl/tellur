use sysinfo::System;

/// Total installed system RAM in bytes.
pub fn host_memory_total_bytes() -> u64 {
    let mut system = System::new();
    system.refresh_memory();
    system.total_memory()
}

/// A short CPU summary for startup banners, e.g. `AMD Ryzen 9 7950X (32 threads)`.
pub fn host_cpu_summary() -> String {
    let mut system = System::new();
    system.refresh_cpu_all();
    let cpus = system.cpus();
    let threads = cpus.len();
    let brand = cpus
        .first()
        .map(|cpu| cpu.brand().trim())
        .filter(|brand| !brand.is_empty())
        .unwrap_or("unknown CPU");
    if threads == 0 {
        brand.to_string()
    } else if threads == 1 {
        format!("{brand} (1 thread)")
    } else {
        format!("{brand} ({threads} threads)")
    }
}

#[cfg(test)]
mod tests {
    use super::{host_cpu_summary, host_memory_total_bytes};

    #[test]
    fn host_cpu_summary_is_non_empty() {
        let summary = host_cpu_summary();
        assert!(!summary.is_empty());
    }

    #[test]
    fn host_cpu_summary_includes_cpu_brand() {
        let summary = host_cpu_summary();
        assert!(!summary.starts_with("unknown CPU"));
    }

    #[test]
    fn host_memory_total_bytes_is_non_zero() {
        assert!(host_memory_total_bytes() > 0);
    }
}
