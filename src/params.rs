//! Shared precompute parameters (CPU + GPU uniforms).

use bytemuck::{Pod, Zeroable};

pub const BYTE7_MASK: u64 = 0x00ff_ffff_ffff_ffff;
// One warp/wave keeps this register-heavy scalar DES kernel at higher
// occupancy than the previous 256-thread default on current GPUs.
pub const WORKGROUP_SIZE: u32 = 32;
/// Preferred upper bound on GPU dispatch width (multiple of WORKGROUP_SIZE).
pub const GPU_DISPATCH_CHUNK: u32 = 65_536;

/// Default DES-step budget per GPU submission. At the old 32M setting a real
/// precompute needed ~12,000 tiny submissions. 8B also preserves enough
/// late-chain workgroups to occupy a discrete GPU; override it for
/// slower/browser GPUs.
pub const GPU_TARGET_STEPS_PER_DISPATCH: u64 = 8_000_000_000;

/// `TABLE_INDEX_TO_REDUCTION_OFFSET(table_index) = table_index * 65536`
pub fn reduction_offset_from_table_index(table_index: u32) -> u32 {
    table_index.wrapping_mul(65536)
}

pub fn output_len(chain_len: u64, total_devices: u32) -> u32 {
    let td = total_devices.max(1) as u64;
    (chain_len.saturating_sub(1).div_ceil(td)) as u32
}

/// Approximate chain steps for one full single-device precompute of `chain_len`.
///
/// Exact single-device total is `(chain_len-1)*(chain_len-2)/2` (see
/// [`exact_precompute_steps`]); this older helper is slightly larger and kept for
/// display compatibility.
pub fn approx_chain_steps(chain_len: u64) -> u64 {
    if chain_len < 2 {
        return 0;
    }
    chain_len.saturating_mul(chain_len.saturating_sub(1)) / 2
}

/// DES loop iterations for one absolute work index (matches `precompute_one`).
pub fn steps_for_abs(abs: u32, chain_len: u64, device_num: u32, total_devices: u32) -> u64 {
    let td = total_devices.max(1) as i64;
    let target = (chain_len as i64) - (device_num as i64) - ((abs as i64) * td) - 1;
    if target < 1 {
        return 0;
    }
    // position runs from target .. chain_len-2 inclusive count = (chain_len-1) - target
    ((chain_len as i64) - 1 - target) as u64
}

/// Exact total DES steps for a precompute config (all absolute indices).
pub fn exact_precompute_steps(chain_len: u64, device_num: u32, total_devices: u32) -> u64 {
    let n = output_len(chain_len, total_devices);
    steps_for_abs_range(0, n, chain_len, device_num, total_devices)
}

/// Sum of [`steps_for_abs`] over `abs ∈ [start, start+len)`.
///
/// For valid indices, `steps(abs) = device_num + abs * total_devices` (arithmetic series).
pub fn steps_for_abs_range(
    start: u32,
    len: u32,
    chain_len: u64,
    device_num: u32,
    total_devices: u32,
) -> u64 {
    if len == 0 {
        return 0;
    }
    let td = total_devices.max(1) as u64;
    let dn = device_num as u64;
    // Last abs with target >= 1: abs*td <= chain_len - device_num - 2
    let max_live = if chain_len <= dn + 1 {
        0u32
    } else {
        ((chain_len - dn - 2) / td) as u32
    };
    let end = start.saturating_add(len).saturating_sub(1);
    if start > max_live {
        return 0;
    }
    let last = end.min(max_live);
    let n = (last - start + 1) as u64;
    // sum_{i=0}^{n-1} (dn + (start+i)*td) = n*dn + td*(n*start + n*(n-1)/2)
    let start_u = start as u64;
    n * dn + td * (n * start_u + n * (n - 1) / 2)
}

/// Adaptive dispatch width: large early (cheap abs), small late (expensive abs).
pub fn gpu_dispatch_block_len(
    abs_start: u32,
    remaining: u32,
    chain_len: u64,
    device_num: u32,
    total_devices: u32,
    max_chunk: u32,
    target_steps: u64,
) -> u32 {
    if remaining == 0 {
        return 0;
    }
    let max_chunk = max_chunk.max(1).min(remaining);
    let s0 = steps_for_abs(abs_start, chain_len, device_num, total_devices).max(1);
    let rough = (target_steps.max(1) / s0).max(1).min(u64::from(max_chunk)) as u32;
    let mut len = rough.min(remaining);
    if len >= WORKGROUP_SIZE {
        len = (len / WORKGROUP_SIZE) * WORKGROUP_SIZE;
    }
    len.max(1).min(remaining)
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct GpuParams {
    pub hash_lo: u32,
    pub hash_hi: u32,
    pub reduction_offset: u32,
    pub chain_len: u32,
    pub device_num: u32,
    pub total_devices: u32,
    pub exec_block_scaler: u32,
    pub output_len: u32,
}

impl GpuParams {
    pub fn new(
        hash: [u8; 8],
        reduction_offset: u32,
        chain_len: u64,
        device_num: u32,
        total_devices: u32,
        exec_block_scaler: u32,
        output_len: u32,
    ) -> Self {
        let hash_u64 = u64::from_le_bytes(hash);
        Self {
            hash_lo: hash_u64 as u32,
            hash_hi: (hash_u64 >> 32) as u32,
            reduction_offset,
            chain_len: chain_len as u32,
            device_num,
            total_devices: total_devices.max(1),
            exec_block_scaler,
            output_len,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PrecomputeConfig {
    pub hash: [u8; 8],
    pub table_index: u32,
    pub chain_len: u64,
    pub device_num: u32,
    pub total_devices: u32,
    pub workgroup_size: u32,
    pub gpu_target_steps_per_dispatch: u64,
}

impl PrecomputeConfig {
    pub fn reduction_offset(&self) -> u32 {
        reduction_offset_from_table_index(self.table_index)
    }

    pub fn output_len(&self) -> u32 {
        output_len(self.chain_len, self.total_devices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_range_matches_per_abs_sum() {
        let chain_len = 10_000u64;
        let dn = 0u32;
        let td = 1u32;
        let n = output_len(chain_len, td);
        let expect: u64 = (0..n).map(|a| steps_for_abs(a, chain_len, dn, td)).sum();
        assert_eq!(steps_for_abs_range(0, n, chain_len, dn, td), expect);
        assert_eq!(exact_precompute_steps(chain_len, dn, td), expect);

        let start = 1234u32;
        let len = 500u32;
        let expect: u64 = (start..start + len)
            .map(|a| steps_for_abs(a, chain_len, dn, td))
            .sum();
        assert_eq!(steps_for_abs_range(start, len, chain_len, dn, td), expect);
    }

    #[test]
    fn production_output_excludes_invalid_zero_target() {
        assert_eq!(output_len(881_689, 1), 881_688);
        assert_eq!(output_len(128, 1), 127);
    }

    #[test]
    fn adaptive_dispatch_shrinks_for_expensive_abs() {
        let chain_len = 881_689u64;
        let early = gpu_dispatch_block_len(
            0,
            100_000,
            chain_len,
            0,
            1,
            GPU_DISPATCH_CHUNK,
            GPU_TARGET_STEPS_PER_DISPATCH,
        );
        let late = gpu_dispatch_block_len(
            800_000,
            50_000,
            chain_len,
            0,
            1,
            GPU_DISPATCH_CHUNK,
            GPU_TARGET_STEPS_PER_DISPATCH,
        );
        assert!(early > late, "early={early} late={late}");
        assert!(late >= 1);
        assert!(early <= GPU_DISPATCH_CHUNK);
    }
}
