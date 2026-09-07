//! Bitsliced NetNTLMv1 for precompute.
//!
//! - `x86_64`: [`fast-des`](https://crates.io/crates/fast-des) 512-wide SIMD path
//! - other arches: portable 64-lane `u64` bitslice (`bs_des` / `bs_sboxes`)
//!
//! Challenge plaintext is the Crackalack fixed block `0x1122334455667788`
//! (same post-IP state as scalar `netntlmv1_hash`).

use crate::params::BYTE7_MASK;

/// Fixed NetNTLMv1 challenge used by RainbowCrackalack tables.
pub const NETNTLMV1_CHALLENGE: u64 = 0x1122_3344_5566_7788;

/// Keys hashed per bitslice wave.
pub const BITSLICE_WIDTH: usize = 512;

const BITSLICE_STACK_SIZE: usize = 32 * 1024 * 1024;

/// Run bitsliced DES with enough stack for its large generated temporaries.
pub fn with_bitslice_stack<F, R>(f: F) -> R
where
    F: FnOnce() -> R + Send,
    R: Send,
{
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("ntlmrain-bitslice".into())
            .stack_size(BITSLICE_STACK_SIZE)
            .spawn_scoped(scope, f)
            .expect("spawn bitslice thread")
            .join()
            .expect("bitslice thread panicked")
    })
}

/// Map a 56-bit rainbow index to the 56-bit key word expected by the bitslice path.
#[inline]
pub fn index_to_fast_des_key(index: u64) -> u64 {
    index & BYTE7_MASK
}

/// Convert ciphertext `u64` (DES block as big-endian integer) to the
/// little-endian hash word used by Crackalack `byte7_hash_value`.
#[inline]
pub fn ciphertext_to_hash_le(ct_be: u64) -> u64 {
    u64::from_le_bytes(ct_be.to_be_bytes())
}

#[cfg(target_arch = "x86_64")]
mod imp {
    use fast_des::bitsliced_netntlmv1_simd;

    use super::{BITSLICE_WIDTH, NETNTLMV1_CHALLENGE, ciphertext_to_hash_le};

    const LANES: usize = 64;
    const GROUPS: usize = 8;

    pub fn netntlmv1_bitslice_batch(keys_in: &[u64], out: &mut [u64]) {
        debug_assert!(keys_in.len() <= BITSLICE_WIDTH);
        debug_assert_eq!(keys_in.len(), out.len());
        let n = keys_in.len();
        if n == 0 {
            return;
        }

        let mut keys = [[0u64; LANES]; GROUPS];
        for (i, &k) in keys_in.iter().enumerate() {
            keys[i / LANES][i % LANES] = k;
        }
        let cts = bitsliced_netntlmv1_simd(NETNTLMV1_CHALLENGE, &keys);
        for i in 0..n {
            out[i] = ciphertext_to_hash_le(cts[i / LANES][i % LANES]);
        }
    }
}

#[cfg(not(target_arch = "x86_64"))]
mod imp {
    use crate::bs_des::netntlmv1_64;

    use super::{BITSLICE_WIDTH, NETNTLMV1_CHALLENGE, ciphertext_to_hash_le};

    const LANES: usize = 64;

    pub fn netntlmv1_bitslice_batch(keys_in: &[u64], out: &mut [u64]) {
        debug_assert!(keys_in.len() <= BITSLICE_WIDTH);
        debug_assert_eq!(keys_in.len(), out.len());
        let n = keys_in.len();
        if n == 0 {
            return;
        }

        let mut group = [0u64; LANES];
        let mut offset = 0;
        while offset < n {
            let take = (n - offset).min(LANES);
            group[..take].copy_from_slice(&keys_in[offset..offset + take]);
            for slot in &mut group[take..] {
                *slot = 0;
            }
            let cts = netntlmv1_64(NETNTLMV1_CHALLENGE, &group);
            for i in 0..take {
                out[offset + i] = ciphertext_to_hash_le(cts[i]);
            }
            offset += take;
        }
    }
}

pub use imp::netntlmv1_bitslice_batch;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{byte7_index_to_plaintext, netntlmv1_hash};

    #[test]
    fn bitslice_matches_scalar_sample() {
        let k = 0x0088_46F7_EAEE_8FB1_u64;
        let out = with_bitslice_stack(|| {
            let mut out = [0u64; 1];
            netntlmv1_bitslice_batch(&[k], &mut out);
            out[0]
        });
        assert_eq!(out, ciphertext_to_hash_le(0x727B_4E35_F947_129E));

        let p = [
            ((k >> 48) & 0xff) as u8,
            ((k >> 40) & 0xff) as u8,
            ((k >> 32) & 0xff) as u8,
            ((k >> 24) & 0xff) as u8,
            ((k >> 16) & 0xff) as u8,
            ((k >> 8) & 0xff) as u8,
            (k & 0xff) as u8,
        ];
        let scalar = u64::from_le_bytes(netntlmv1_hash(&p));
        assert_eq!(out, scalar, "bitslice vs scalar for README key");
    }

    #[test]
    fn bitslice_matches_scalar_random_indices() {
        let mut keys = Vec::new();
        let mut expect = Vec::new();
        for i in 0u64..128 {
            let idx = (i.wrapping_mul(0x9e37_79b9_7f4a_7c15)) & BYTE7_MASK;
            keys.push(index_to_fast_des_key(idx));
            let p = byte7_index_to_plaintext(idx);
            expect.push(u64::from_le_bytes(netntlmv1_hash(&p)));
        }
        let out = with_bitslice_stack(|| {
            let mut out = vec![0u64; keys.len()];
            netntlmv1_bitslice_batch(&keys, &mut out);
            out
        });
        assert_eq!(out, expect);
    }
}
