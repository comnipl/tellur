//! Process-wide cache and GPU memory budgets.
//!
//! `TELLUR_CACHE_RAM` limits bytes held by Tellur-managed CPU-side caches.
//! `TELLUR_VRAM` is a best-effort budget for GPU buffers/textures allocated by
//! Tellur's wgpu backend. Driver-internal allocations are not directly visible,
//! so VRAM accounting intentionally tracks the large allocations we create.

use std::env;
use std::sync::{LazyLock, Mutex};

pub const DEFAULT_CACHE_RAM_BYTES: usize = 1024 * 1024 * 1024;
pub const DEFAULT_VRAM_BYTES: usize = 1024 * 1024 * 1024;

const TELLUR_CACHE_RAM: &str = "TELLUR_CACHE_RAM";
const TELLUR_VRAM: &str = "TELLUR_VRAM";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetResource {
    CacheRam,
    Vram,
}

#[derive(Debug)]
pub struct BudgetReservation {
    resource: BudgetResource,
    bytes: usize,
}

impl BudgetReservation {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Drop for BudgetReservation {
    fn drop(&mut self) {
        release(self.resource, self.bytes);
    }
}

#[derive(Debug, Default)]
struct BudgetUse {
    cache_ram: usize,
    vram: usize,
}

static BUDGET_USE: LazyLock<Mutex<BudgetUse>> = LazyLock::new(|| Mutex::new(BudgetUse::default()));

pub fn configured_cache_ram_bytes() -> usize {
    env_byte_size(TELLUR_CACHE_RAM).unwrap_or(DEFAULT_CACHE_RAM_BYTES)
}

pub fn configured_vram_bytes() -> usize {
    env_byte_size(TELLUR_VRAM).unwrap_or(DEFAULT_VRAM_BYTES)
}

/// Per-cache local capacity: when the user sets `TELLUR_CACHE_RAM`, local cache
/// caps should not exceed it; otherwise they keep their historical defaults.
pub fn cache_ram_capacity(default: usize) -> usize {
    env_byte_size(TELLUR_CACHE_RAM).unwrap_or(default)
}

pub fn try_reserve_cache_ram(bytes: usize) -> Option<BudgetReservation> {
    try_reserve(
        BudgetResource::CacheRam,
        bytes,
        configured_cache_ram_bytes(),
    )
}

pub fn try_reserve_vram(bytes: usize) -> Option<BudgetReservation> {
    try_reserve(BudgetResource::Vram, bytes, configured_vram_bytes())
}

pub fn cache_ram_used_bytes() -> usize {
    BUDGET_USE.lock().map(|usage| usage.cache_ram).unwrap_or(0)
}

pub fn vram_used_bytes() -> usize {
    BUDGET_USE.lock().map(|usage| usage.vram).unwrap_or(0)
}

fn try_reserve(resource: BudgetResource, bytes: usize, limit: usize) -> Option<BudgetReservation> {
    if bytes > limit {
        return None;
    }
    let mut usage = BUDGET_USE.lock().ok()?;
    let used = match resource {
        BudgetResource::CacheRam => &mut usage.cache_ram,
        BudgetResource::Vram => &mut usage.vram,
    };
    if used.saturating_add(bytes) > limit {
        return None;
    }
    *used += bytes;
    Some(BudgetReservation { resource, bytes })
}

fn release(resource: BudgetResource, bytes: usize) {
    if bytes == 0 {
        return;
    }
    if let Ok(mut usage) = BUDGET_USE.lock() {
        let used = match resource {
            BudgetResource::CacheRam => &mut usage.cache_ram,
            BudgetResource::Vram => &mut usage.vram,
        };
        *used = used.saturating_sub(bytes);
    }
}

fn env_byte_size(name: &str) -> Option<usize> {
    match env::var(name) {
        Ok(value) => match parse_byte_size(&value) {
            Some(bytes) => Some(bytes),
            None => {
                eprintln!("ignoring invalid {name}={value:?}; expected bytes or k/m/g suffix");
                None
            }
        },
        Err(_) => None,
    }
}

fn parse_byte_size(raw: &str) -> Option<usize> {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() || value == "auto" {
        return None;
    }
    let split = value
        .find(|c: char| !c.is_ascii_digit() && c != '_')
        .unwrap_or(value.len());
    let number = value[..split].replace('_', "");
    if number.is_empty() {
        return None;
    }
    let base: usize = number.parse().ok()?;
    let suffix = value[split..].trim();
    let scale = match suffix {
        "" | "b" => 1usize,
        "k" | "kb" | "kib" => 1024usize,
        "m" | "mb" | "mib" => 1024usize.pow(2),
        "g" | "gb" | "gib" => 1024usize.pow(3),
        _ => return None,
    };
    base.checked_mul(scale)
}

#[cfg(test)]
mod tests {
    use super::parse_byte_size;

    #[test]
    fn parses_byte_sizes() {
        assert_eq!(parse_byte_size("512"), Some(512));
        assert_eq!(parse_byte_size("1k"), Some(1024));
        assert_eq!(parse_byte_size("2MiB"), Some(2 * 1024 * 1024));
        assert_eq!(parse_byte_size("3_gb"), Some(3 * 1024 * 1024 * 1024));
        assert_eq!(parse_byte_size("auto"), None);
        assert_eq!(parse_byte_size("nope"), None);
    }
}
