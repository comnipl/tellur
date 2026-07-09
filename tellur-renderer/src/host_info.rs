use sysinfo::System;

/// A short CPU summary for startup banners, e.g. `AMD Ryzen 9 7950X (32 threads)`.
pub fn host_cpu_summary() -> String {
    let system = System::new();
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
    use super::host_cpu_summary;

    #[test]
    fn host_cpu_summary_is_non_empty() {
        let summary = host_cpu_summary();
        assert!(!summary.is_empty());
    }
}
