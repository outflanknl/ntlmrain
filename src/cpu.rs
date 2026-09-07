//! CPU reference for netntlmv1_byte#7-7 precompute.
//!
//! Port of RainbowCrackalack OpenCL:
//! - `CL/netntlmv1_byte7_functions.cl`
//! - scalar DES / `netntlmv1_hash` at the end of `CL/netntlmv1.cl`
//! - `CL/precompute_netntlmv1_byte7.cl`

#![allow(clippy::identity_op, clippy::manual_rotate)]

use crate::params::{BYTE7_MASK, PrecomputeConfig, WORKGROUP_SIZE};

const SB1: [u32; 64] = [
    0x01010400, 0x00000000, 0x00010000, 0x01010404, 0x01010004, 0x00010404, 0x00000004, 0x00010000,
    0x00000400, 0x01010400, 0x01010404, 0x00000400, 0x01000404, 0x01010004, 0x01000000, 0x00000004,
    0x00000404, 0x01000400, 0x01000400, 0x00010400, 0x00010400, 0x01010000, 0x01010000, 0x01000404,
    0x00010004, 0x01000004, 0x01000004, 0x00010004, 0x00000000, 0x00000404, 0x00010404, 0x01000000,
    0x00010000, 0x01010404, 0x00000004, 0x01010000, 0x01010400, 0x01000000, 0x01000000, 0x00000400,
    0x01010004, 0x00010000, 0x00010400, 0x01000004, 0x00000400, 0x00000004, 0x01000404, 0x00010404,
    0x01010404, 0x00010004, 0x01010000, 0x01000404, 0x01000004, 0x00000404, 0x00010404, 0x01010400,
    0x00000404, 0x01000400, 0x01000400, 0x00000000, 0x00010004, 0x00010400, 0x00000000, 0x01010004,
];

const SB2: [u32; 64] = [
    0x80108020, 0x80008000, 0x00008000, 0x00108020, 0x00100000, 0x00000020, 0x80100020, 0x80008020,
    0x80000020, 0x80108020, 0x80108000, 0x80000000, 0x80008000, 0x00100000, 0x00000020, 0x80100020,
    0x00108000, 0x00100020, 0x80008020, 0x00000000, 0x80000000, 0x00008000, 0x00108020, 0x80100000,
    0x00100020, 0x80000020, 0x00000000, 0x00108000, 0x00008020, 0x80108000, 0x80100000, 0x00008020,
    0x00000000, 0x00108020, 0x80100020, 0x00100000, 0x80008020, 0x80100000, 0x80108000, 0x00008000,
    0x80100000, 0x80008000, 0x00000020, 0x80108020, 0x00108020, 0x00000020, 0x00008000, 0x80000000,
    0x00008020, 0x80108000, 0x00100000, 0x80000020, 0x00100020, 0x80008020, 0x80000020, 0x00100020,
    0x00108000, 0x00000000, 0x80008000, 0x00008020, 0x80000000, 0x80100020, 0x80108020, 0x00108000,
];

const SB3: [u32; 64] = [
    0x00000208, 0x08020200, 0x00000000, 0x08020008, 0x08000200, 0x00000000, 0x00020208, 0x08000200,
    0x00020008, 0x08000008, 0x08000008, 0x00020000, 0x08020208, 0x00020008, 0x08020000, 0x00000208,
    0x08000000, 0x00000008, 0x08020200, 0x00000200, 0x00020200, 0x08020000, 0x08020008, 0x00020208,
    0x08000208, 0x00020200, 0x00020000, 0x08000208, 0x00000008, 0x08020208, 0x00000200, 0x08000000,
    0x08020200, 0x08000000, 0x00020008, 0x00000208, 0x00020000, 0x08020200, 0x08000200, 0x00000000,
    0x00000200, 0x00020008, 0x08020208, 0x08000200, 0x08000008, 0x00000200, 0x00000000, 0x08020008,
    0x08000208, 0x00020000, 0x08000000, 0x08020208, 0x00000008, 0x00020208, 0x00020200, 0x08000008,
    0x08020000, 0x08000208, 0x00000208, 0x08020000, 0x00020208, 0x00000008, 0x08020008, 0x00020200,
];

const SB4: [u32; 64] = [
    0x00802001, 0x00002081, 0x00002081, 0x00000080, 0x00802080, 0x00800081, 0x00800001, 0x00002001,
    0x00000000, 0x00802000, 0x00802000, 0x00802081, 0x00000081, 0x00000000, 0x00800080, 0x00800001,
    0x00000001, 0x00002000, 0x00800000, 0x00802001, 0x00000080, 0x00800000, 0x00002001, 0x00002080,
    0x00800081, 0x00000001, 0x00002080, 0x00800080, 0x00002000, 0x00802080, 0x00802081, 0x00000081,
    0x00800080, 0x00800001, 0x00802000, 0x00802081, 0x00000081, 0x00000000, 0x00000000, 0x00802000,
    0x00002080, 0x00800080, 0x00800081, 0x00000001, 0x00802001, 0x00002081, 0x00002081, 0x00000080,
    0x00802081, 0x00000081, 0x00000001, 0x00002000, 0x00800001, 0x00002001, 0x00802080, 0x00800081,
    0x00002001, 0x00002080, 0x00800000, 0x00802001, 0x00000080, 0x00800000, 0x00002000, 0x00802080,
];

const SB5: [u32; 64] = [
    0x00000100, 0x02080100, 0x02080000, 0x42000100, 0x00080000, 0x00000100, 0x40000000, 0x02080000,
    0x40080100, 0x00080000, 0x02000100, 0x40080100, 0x42000100, 0x42080000, 0x00080100, 0x40000000,
    0x02000000, 0x40080000, 0x40080000, 0x00000000, 0x40000100, 0x42080100, 0x42080100, 0x02000100,
    0x42080000, 0x40000100, 0x00000000, 0x42000000, 0x02080100, 0x02000000, 0x42000000, 0x00080100,
    0x00080000, 0x42000100, 0x00000100, 0x02000000, 0x40000000, 0x02080000, 0x42000100, 0x40080100,
    0x02000100, 0x40000000, 0x42080000, 0x02080100, 0x40080100, 0x00000100, 0x02000000, 0x42080000,
    0x42080100, 0x00080100, 0x42000000, 0x42080100, 0x02080000, 0x00000000, 0x40080000, 0x42000000,
    0x00080100, 0x02000100, 0x40000100, 0x00080000, 0x00000000, 0x40080000, 0x02080100, 0x40000100,
];

const SB6: [u32; 64] = [
    0x20000010, 0x20400000, 0x00004000, 0x20404010, 0x20400000, 0x00000010, 0x20404010, 0x00400000,
    0x20004000, 0x00404010, 0x00400000, 0x20000010, 0x00400010, 0x20004000, 0x20000000, 0x00004010,
    0x00000000, 0x00400010, 0x20004010, 0x00004000, 0x00404000, 0x20004010, 0x00000010, 0x20400010,
    0x20400010, 0x00000000, 0x00404010, 0x20404000, 0x00004010, 0x00404000, 0x20404000, 0x20000000,
    0x20004000, 0x00000010, 0x20400010, 0x00404000, 0x20404010, 0x00400000, 0x00004010, 0x20000010,
    0x00400000, 0x20004000, 0x20000000, 0x00004010, 0x20000010, 0x20404010, 0x00404000, 0x20400000,
    0x00404010, 0x20404000, 0x00000000, 0x20400010, 0x00000010, 0x00004000, 0x20400000, 0x00404010,
    0x00004000, 0x00400010, 0x20004010, 0x00000000, 0x20404000, 0x20000000, 0x00400010, 0x20004010,
];

const SB7: [u32; 64] = [
    0x00200000, 0x04200002, 0x04000802, 0x00000000, 0x00000800, 0x04000802, 0x00200802, 0x04200800,
    0x04200802, 0x00200000, 0x00000000, 0x04000002, 0x00000002, 0x04000000, 0x04200002, 0x00000802,
    0x04000800, 0x00200802, 0x00200002, 0x04000800, 0x04000002, 0x04200000, 0x04200800, 0x00200002,
    0x04200000, 0x00000800, 0x00000802, 0x04200802, 0x00200800, 0x00000002, 0x04000000, 0x00200800,
    0x04000000, 0x00200800, 0x00200000, 0x04000802, 0x04000802, 0x04200002, 0x04200002, 0x00000002,
    0x00200002, 0x04000000, 0x04000800, 0x00200000, 0x04200800, 0x00000802, 0x00200802, 0x04200800,
    0x00000802, 0x04000002, 0x04200802, 0x04200000, 0x00200800, 0x00000000, 0x00000002, 0x04200802,
    0x00000000, 0x00200802, 0x04200000, 0x00000800, 0x04000002, 0x04000800, 0x00000800, 0x00200002,
];

const SB8: [u32; 64] = [
    0x10001040, 0x00001000, 0x00040000, 0x10041040, 0x10000000, 0x10001040, 0x00000040, 0x10000000,
    0x00040040, 0x10040000, 0x10041040, 0x00041000, 0x10041000, 0x00041040, 0x00001000, 0x00000040,
    0x10040000, 0x10000040, 0x10001000, 0x00001040, 0x00041000, 0x00040040, 0x10040040, 0x10041000,
    0x00001040, 0x00000000, 0x00000000, 0x10040040, 0x10000040, 0x10001000, 0x00041040, 0x00040000,
    0x00041040, 0x00040000, 0x10041000, 0x00001000, 0x00000040, 0x10040040, 0x00001000, 0x00041040,
    0x10001000, 0x00000040, 0x10000040, 0x10040000, 0x10040040, 0x10000000, 0x00040000, 0x10001040,
    0x00000000, 0x10041040, 0x00040040, 0x10000040, 0x10040000, 0x10001000, 0x10001040, 0x00000000,
    0x10041040, 0x00041000, 0x00041000, 0x00001040, 0x00001040, 0x00040040, 0x10000000, 0x10041000,
];

const LHS: [u32; 16] = [
    0x00000000, 0x00000001, 0x00000100, 0x00000101, 0x00010000, 0x00010001, 0x00010100, 0x00010101,
    0x01000000, 0x01000001, 0x01000100, 0x01000101, 0x01010000, 0x01010001, 0x01010100, 0x01010101,
];

const RHS: [u32; 16] = [
    0x00000000, 0x01000000, 0x00010000, 0x01010000, 0x00000100, 0x01000100, 0x00010100, 0x01010100,
    0x00000001, 0x01000001, 0x00010001, 0x01010001, 0x00000101, 0x01000101, 0x00010101, 0x01010101,
];

#[inline]
fn get_uint32_be(b: &[u8], i: usize) -> u32 {
    ((b[i] as u32) << 24) | ((b[i + 1] as u32) << 16) | ((b[i + 2] as u32) << 8) | (b[i + 3] as u32)
}

#[inline]
fn put_uint32_be(n: u32, b: &mut [u8], i: usize) {
    b[i] = (n >> 24) as u8;
    b[i + 1] = (n >> 16) as u8;
    b[i + 2] = (n >> 8) as u8;
    b[i + 3] = n as u8;
}

fn des_ecb_setkey(sk: &mut [u32; 32], key: &[u8; 8]) {
    let mut x = get_uint32_be(key, 0);
    let mut y = get_uint32_be(key, 4);

    let mut t = ((y >> 4) ^ x) & 0x0F0F0F0F;
    x ^= t;
    y ^= t << 4;
    t = (y ^ x) & 0x10101010;
    x ^= t;
    y ^= t;

    x = (LHS[(x & 0xF) as usize] << 3)
        | (LHS[((x >> 8) & 0xF) as usize] << 2)
        | (LHS[((x >> 16) & 0xF) as usize] << 1)
        | LHS[((x >> 24) & 0xF) as usize]
        | (LHS[((x >> 5) & 0xF) as usize] << 7)
        | (LHS[((x >> 13) & 0xF) as usize] << 6)
        | (LHS[((x >> 21) & 0xF) as usize] << 5)
        | (LHS[((x >> 29) & 0xF) as usize] << 4);

    y = (RHS[((y >> 1) & 0xF) as usize] << 3)
        | (RHS[((y >> 9) & 0xF) as usize] << 2)
        | (RHS[((y >> 17) & 0xF) as usize] << 1)
        | RHS[((y >> 25) & 0xF) as usize]
        | (RHS[((y >> 4) & 0xF) as usize] << 7)
        | (RHS[((y >> 12) & 0xF) as usize] << 6)
        | (RHS[((y >> 20) & 0xF) as usize] << 5)
        | (RHS[((y >> 28) & 0xF) as usize] << 4);

    x &= 0x0FFFFFFF;
    y &= 0x0FFFFFFF;

    let mut sk_i = 0usize;
    for i in 0..16 {
        if i < 2 || i == 8 || i == 15 {
            x = ((x << 1) | (x >> 27)) & 0x0FFFFFFF;
            y = ((y << 1) | (y >> 27)) & 0x0FFFFFFF;
        } else {
            x = ((x << 2) | (x >> 26)) & 0x0FFFFFFF;
            y = ((y << 2) | (y >> 26)) & 0x0FFFFFFF;
        }

        sk[sk_i] = ((x << 4) & 0x24000000)
            | ((x << 28) & 0x10000000)
            | ((x << 14) & 0x08000000)
            | ((x << 18) & 0x02080000)
            | ((x << 6) & 0x01000000)
            | ((x << 9) & 0x00200000)
            | ((x >> 1) & 0x00100000)
            | ((x << 10) & 0x00040000)
            | ((x << 2) & 0x00020000)
            | ((x >> 10) & 0x00010000)
            | ((y >> 13) & 0x00002000)
            | ((y >> 4) & 0x00001000)
            | ((y << 6) & 0x00000800)
            | ((y >> 1) & 0x00000400)
            | ((y >> 14) & 0x00000200)
            | (y & 0x00000100)
            | ((y >> 5) & 0x00000020)
            | ((y >> 10) & 0x00000010)
            | ((y >> 3) & 0x00000008)
            | ((y >> 18) & 0x00000004)
            | ((y >> 26) & 0x00000002)
            | ((y >> 24) & 0x00000001);
        sk_i += 1;

        sk[sk_i] = ((x << 15) & 0x20000000)
            | ((x << 17) & 0x10000000)
            | ((x << 10) & 0x08000000)
            | ((x << 22) & 0x04000000)
            | ((x >> 2) & 0x02000000)
            | ((x << 1) & 0x01000000)
            | ((x << 16) & 0x00200000)
            | ((x << 11) & 0x00100000)
            | ((x << 3) & 0x00080000)
            | ((x >> 6) & 0x00040000)
            | ((x << 15) & 0x00020000)
            | ((x >> 4) & 0x00010000)
            | ((y >> 2) & 0x00002000)
            | ((y << 8) & 0x00001000)
            | ((y >> 14) & 0x00000808)
            | ((y >> 9) & 0x00000400)
            | (y & 0x00000200)
            | ((y << 7) & 0x00000100)
            | ((y >> 7) & 0x00000020)
            | ((y >> 3) & 0x00000011)
            | ((y << 2) & 0x00000004)
            | ((y >> 21) & 0x00000002);
        sk_i += 1;
    }
}

fn des_ecb_setkey_56(sk: &mut [u32; 32], key56: &[u8; 7]) {
    let mut key = [0u8; 8];
    key[0] = ((key56[0] >> 1) & 0x7f) << 1;
    key[1] = (((key56[0] & 0x01) << 6) | ((key56[1] >> 2) & 0x3f)) << 1;
    key[2] = (((key56[1] & 0x03) << 5) | ((key56[2] >> 3) & 0x1f)) << 1;
    key[3] = (((key56[2] & 0x07) << 4) | ((key56[3] >> 4) & 0x0f)) << 1;
    key[4] = (((key56[3] & 0x0f) << 3) | ((key56[4] >> 5) & 0x07)) << 1;
    key[5] = (((key56[4] & 0x1f) << 2) | ((key56[5] >> 6) & 0x03)) << 1;
    key[6] = (((key56[5] & 0x3f) << 1) | ((key56[6] >> 7) & 0x01)) << 1;
    key[7] = (key56[6] & 0x7f) << 1;
    des_ecb_setkey(sk, &key);
}

#[inline]
fn des_round(sk: &[u32; 32], sk_base: &mut usize, x: u32, y: &mut u32) {
    let mut t = sk[*sk_base] ^ x;
    *sk_base += 1;
    *y ^= SB8[(t & 0x3F) as usize]
        ^ SB6[((t >> 8) & 0x3F) as usize]
        ^ SB4[((t >> 16) & 0x3F) as usize]
        ^ SB2[((t >> 24) & 0x3F) as usize];

    t = sk[*sk_base] ^ ((x << 28) | (x >> 4));
    *sk_base += 1;
    *y ^= SB7[(t & 0x3F) as usize]
        ^ SB5[((t >> 8) & 0x3F) as usize]
        ^ SB3[((t >> 16) & 0x3F) as usize]
        ^ SB1[((t >> 24) & 0x3F) as usize];
}

fn des_fp(x: &mut u32, y: &mut u32) {
    *x = ((*x << 31) | (*x >> 1)) & 0xFFFFFFFF;
    let mut t = (*x ^ *y) & 0xAAAAAAAA;
    *x ^= t;
    *y ^= t;
    *y = ((*y << 31) | (*y >> 1)) & 0xFFFFFFFF;
    t = ((*y >> 8) ^ *x) & 0x00FF00FF;
    *x ^= t;
    *y ^= t << 8;
    t = ((*y >> 2) ^ *x) & 0x33333333;
    *x ^= t;
    *y ^= t << 2;
    t = ((*x >> 16) ^ *y) & 0x0000FFFF;
    *y ^= t;
    *x ^= t << 16;
    t = ((*x >> 4) ^ *y) & 0x0F0F0F0F;
    *y ^= t;
    *x ^= t << 4;
}

/// NetNTLMv1 DES with fixed post-IP challenge state from OpenCL (`1122334455667788`).
pub fn netntlmv1_hash(plaintext7: &[u8; 7]) -> [u8; 8] {
    let mut sk = [0u32; 32];
    des_ecb_setkey_56(&mut sk, plaintext7);

    // Fixed state after IP of plaintext "\x11\x22\x33\x44\x55\x66\x77\x88".
    let mut x: u32 = 0xf0aaf0aa;
    let mut y: u32 = 0x00cd00cd;
    let mut sk_i = 0usize;
    for _ in 0..8 {
        des_round(&sk, &mut sk_i, y, &mut x);
        des_round(&sk, &mut sk_i, x, &mut y);
    }
    des_fp(&mut y, &mut x);

    let mut out = [0u8; 8];
    put_uint32_be(y, &mut out, 0);
    put_uint32_be(x, &mut out, 4);
    out
}

/// Expand seven bytes of NTLM plaintext material into the eight-byte DES key
/// representation used by NetNTLMv1. The low parity-bit positions are set;
/// DES itself ignores those bits.
pub fn expand_des_key(plaintext7: &[u8; 7]) -> [u8; 8] {
    let mut key = [0u8; 8];
    key[0] = plaintext7[0] & 0xfe;
    key[1] = ((plaintext7[0] << 7) | (plaintext7[1] >> 1)) & 0xfe;
    key[2] = ((plaintext7[1] << 6) | (plaintext7[2] >> 2)) & 0xfe;
    key[3] = ((plaintext7[2] << 5) | (plaintext7[3] >> 3)) & 0xfe;
    key[4] = ((plaintext7[3] << 4) | (plaintext7[4] >> 4)) & 0xfe;
    key[5] = ((plaintext7[4] << 3) | (plaintext7[5] >> 5)) & 0xfe;
    key[6] = ((plaintext7[5] << 2) | (plaintext7[6] >> 6)) & 0xfe;
    key[7] = (plaintext7[6] << 1) & 0xfe;
    key.iter_mut().for_each(|byte| *byte |= 1);
    key
}

pub fn byte7_index_to_plaintext(index: u64) -> [u8; 7] {
    [
        (index >> 48) as u8,
        (index >> 40) as u8,
        (index >> 32) as u8,
        (index >> 24) as u8,
        (index >> 16) as u8,
        (index >> 8) as u8,
        index as u8,
    ]
}

pub fn byte7_hash_value(hash: &[u8; 8]) -> u64 {
    u64::from_le_bytes(*hash)
}

pub fn byte7_hash_to_index(hash: &[u8; 8], reduction_offset: u32, position: u32) -> u64 {
    (byte7_hash_value(hash)
        .wrapping_add(reduction_offset as u64)
        .wrapping_add(position as u64))
        & BYTE7_MASK
}

/// One work-item of `precompute_netntlmv1_byte7`.
pub fn precompute_one(
    hash: &[u8; 8],
    reduction_offset: u32,
    chain_len: u64,
    device_num: u32,
    total_devices: u32,
    exec_block_scaler: u32,
    gid: u32,
) -> u64 {
    let total_devices = total_devices.max(1);
    let target = (chain_len as i64)
        - (device_num as i64)
        - (((gid as i64) + (exec_block_scaler as i64)) * (total_devices as i64))
        - 1;
    if target < 1 {
        return 0;
    }

    let mut index = (byte7_hash_value(hash)
        .wrapping_add(reduction_offset as u64)
        .wrapping_add((target as u64).wrapping_sub(1)))
        & BYTE7_MASK;

    let chain_len_u = chain_len as u32;
    let mut position = target as u32;
    while position < chain_len_u.saturating_sub(1) {
        let plaintext = byte7_index_to_plaintext(index);
        let h = netntlmv1_hash(&plaintext);
        index = byte7_hash_to_index(&h, reduction_offset, position);
        position += 1;
    }
    index
}

/// Progress snapshot for CPU precompute (step-weighted).
#[derive(Clone, Copy, Debug)]
pub struct PrecomputeProgress {
    pub indices_done: u32,
    pub indices_total: u32,
    pub steps_done: u64,
    pub steps_total: u64,
}

/// Full precompute for one device (matches OpenCL host `output_len` layout).
///
/// Absolute index `abs` uses the same target formula as OpenCL
/// `gid + exec_block_scaler == abs`.
pub fn precompute(config: &PrecomputeConfig) -> Vec<u64> {
    precompute_with_progress(config, |_| {})
}

/// Like [`precompute`], invoking `progress` periodically with step-weighted totals.
///
/// Uses 512-wide bitsliced NetNTLMv1 (`fast-des` on x86_64, portable elsewhere)
/// with wave scheduling over cost-balanced Rayon batches. Falls back to scalar
/// [`precompute_one`] only inside tests and for trivial sizes.
pub fn precompute_with_progress<F>(config: &PrecomputeConfig, progress: F) -> Vec<u64>
where
    F: Fn(PrecomputeProgress) + Sync,
{
    use crate::{
        bitslice::BITSLICE_WIDTH,
        cpu_schedule::weighted_batch_ranges,
        params::{exact_precompute_steps, steps_for_abs},
    };
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    let reduction_offset = config.reduction_offset();
    let out_len = config.output_len();
    let steps_total =
        exact_precompute_steps(config.chain_len, config.device_num, config.total_devices);
    let indices_done = AtomicU32::new(0);
    let steps_done = AtomicU64::new(0);
    let report = |item_steps: u64, n_indices: u32| {
        let n = indices_done.fetch_add(n_indices, Ordering::Relaxed) + n_indices;
        let s = steps_done.fetch_add(item_steps, Ordering::Relaxed) + item_steps;
        progress(PrecomputeProgress {
            indices_done: n.min(out_len),
            indices_total: out_len,
            steps_done: s.min(steps_total),
            steps_total,
        });
    };

    let ranges = weighted_batch_ranges(
        out_len as usize,
        rayon::current_num_threads(),
        BITSLICE_WIDTH,
        |abs| {
            steps_for_abs(
                abs as u32,
                config.chain_len,
                config.device_num,
                config.total_devices,
            )
        },
    );

    let batches = ranges
        .into_par_iter()
        .map(|range| {
            precompute_chunk_bitslice(
                config,
                reduction_offset,
                range.start as u32,
                range.len(),
                &report,
            )
        })
        .collect::<Vec<_>>();
    batches.into_iter().flatten().collect()
}

struct WaveState {
    abs_local: u32,
    index: u64,
    position: u32,
    end_pos: u32, // chain_len - 1; stop when position >= end_pos
}

fn precompute_chunk_bitslice<F>(
    config: &PrecomputeConfig,
    reduction_offset: u32,
    abs_base: u32,
    out_len: usize,
    report: &F,
) -> Vec<u64>
where
    F: Fn(u64, u32) + Sync,
{
    use crate::bitslice::{
        BITSLICE_WIDTH, index_to_fast_des_key, netntlmv1_bitslice_batch, with_bitslice_stack,
    };

    // Wave loop (large stack on x86_64 for fast-des temps).
    let chain_len = config.chain_len;
    let device_num = config.device_num;
    let td = config.total_devices.max(1);
    let hash = config.hash;
    // Publish progress from inside the bitslice worker. Deferring these events
    // until a complete Rayon chunk returns can leave long production runs
    // apparently idle for minutes.
    with_bitslice_stack(move || {
        let mut out_chunk = vec![0u64; out_len];
        let mut pending_steps = 0u64;
        let mut pending_indices = 0u32;
        let mut last_report = std::time::Instant::now();
        let mut push_report = |steps: u64, idxs: u32, force: bool| {
            pending_steps = pending_steps.saturating_add(steps);
            pending_indices = pending_indices.saturating_add(idxs);
            if (force || last_report.elapsed() >= std::time::Duration::from_millis(250))
                && (pending_steps > 0 || pending_indices > 0)
            {
                report(pending_steps, pending_indices);
                pending_steps = 0;
                pending_indices = 0;
                last_report = std::time::Instant::now();
            }
        };

        let chain_len_u = chain_len as u32;
        let end_pos = chain_len_u.saturating_sub(1);
        let hash_u64 = u64::from_le_bytes(hash);

        let mut active: Vec<WaveState> = Vec::with_capacity(out_len);
        let mut finished_indices: u32 = 0;

        for local in 0..out_len as u32 {
            let abs = abs_base + local;
            let target =
                (chain_len as i64) - (device_num as i64) - ((abs as i64) * (td as i64)) - 1;
            if target < 1 {
                out_chunk[local as usize] = 0;
                finished_indices += 1;
                continue;
            }
            let target_u = target as u32;
            let index = (hash_u64
                .wrapping_add(reduction_offset as u64)
                .wrapping_add((target_u as u64).wrapping_sub(1)))
                & BYTE7_MASK;
            if target_u >= end_pos {
                out_chunk[local as usize] = index;
                finished_indices += 1;
                continue;
            }
            active.push(WaveState {
                abs_local: local,
                index,
                position: target_u,
                end_pos,
            });
        }
        push_report(0, finished_indices, false);

        let mut keys = vec![0u64; BITSLICE_WIDTH];
        let mut hashes = vec![0u64; BITSLICE_WIDTH];

        while !active.is_empty() {
            let active_len = active.len();
            let mut read = 0usize;
            let mut write = 0usize;
            let mut sweep_steps = 0u64;
            let mut sweep_done = 0u32;
            while read < active_len {
                let n = (active_len - read).min(BITSLICE_WIDTH);
                for i in 0..n {
                    keys[i] = index_to_fast_des_key(active[read + i].index);
                }
                netntlmv1_bitslice_batch(&keys[..n], &mut hashes[..n]);

                for (i, &hash) in hashes[..n].iter().enumerate() {
                    let slot = read + i;
                    let st = &mut active[slot];
                    let index = (hash
                        .wrapping_add(reduction_offset as u64)
                        .wrapping_add(st.position as u64))
                        & BYTE7_MASK;
                    let position = st.position + 1;
                    let abs_local = st.abs_local;
                    let end_pos = st.end_pos;
                    sweep_steps += 1;
                    if position >= end_pos {
                        out_chunk[abs_local as usize] = index;
                        sweep_done += 1;
                    } else {
                        if write != slot {
                            active[write] = WaveState {
                                abs_local,
                                index,
                                position,
                                end_pos,
                            };
                        } else {
                            active[write].index = index;
                            active[write].position = position;
                        }
                        write += 1;
                    }
                }
                read += n;
            }
            active.truncate(write);
            push_report(sweep_steps, sweep_done, false);
        }
        push_report(0, 0, true);

        out_chunk
    })
}

/// Convenience with default workgroup size.
pub fn precompute_default(config: &PrecomputeConfig) -> Vec<u64> {
    let mut c = config.clone();
    if c.workgroup_size == 0 {
        c.workgroup_size = WORKGROUP_SIZE;
    }
    precompute(&c)
}

/// Parallel CPU precompute with default workgroup and progress callback.
pub fn precompute_default_with_progress<F>(config: &PrecomputeConfig, progress: F) -> Vec<u64>
where
    F: Fn(PrecomputeProgress) + Sync,
{
    let mut c = config.clone();
    if c.workgroup_size == 0 {
        c.workgroup_size = WORKGROUP_SIZE;
    }
    precompute_with_progress(&c, progress)
}

/// Return whether a rainbow-table index is an exact seven-byte DES key for
/// the fixed NetNTLMv1 challenge and the supplied ciphertext.
pub fn is_exact_des_key_match(index: u64, target: &[u8; 8]) -> bool {
    netntlmv1_hash(&byte7_index_to_plaintext(index)) == *target
}

/// Recover the final two NT-hash bytes used by NetNTLMv1's third DES block.
/// The remaining five bytes of the third seven-byte DES key are zero padding.
pub fn recover_pt3(target: &[u8; 8]) -> Option<[u8; 2]> {
    (0u32..=u16::MAX as u32).find_map(|value| {
        let plaintext = [(value >> 8) as u8, value as u8, 0, 0, 0, 0, 0];
        (netntlmv1_hash(&plaintext) == *target).then_some([plaintext[0], plaintext[1]])
    })
}

/// Assemble the sixteen-byte NT hash from two verified plaintext indexes and DES3.
pub fn assemble_nt_hash(pt1_index: u64, pt2_index: u64, pt3: [u8; 2]) -> [u8; 16] {
    let mut output = [0u8; 16];
    output[..7].copy_from_slice(&byte7_index_to_plaintext(pt1_index));
    output[7..14].copy_from_slice(&byte7_index_to_plaintext(pt2_index));
    output[14..].copy_from_slice(&pt3);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_roundtrip_bytes() {
        let idx = 0x0011_2233_4455_6677u64;
        let p = byte7_index_to_plaintext(idx);
        assert_eq!(p, [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77]);
    }

    #[test]
    fn hash_value_le() {
        let h = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(byte7_hash_value(&h), 0x0807060504030201);
    }

    #[test]
    fn mask_and_reduce() {
        let h = [0xff; 8];
        let idx = byte7_hash_to_index(&h, 0, 0);
        assert_eq!(idx, BYTE7_MASK);
    }

    #[test]
    fn short_chain_deterministic() {
        let cfg = PrecomputeConfig {
            hash: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
            table_index: 0,
            chain_len: 8,
            device_num: 0,
            total_devices: 1,
            workgroup_size: 64,
            gpu_target_steps_per_dispatch: crate::params::GPU_TARGET_STEPS_PER_DISPATCH,
        };
        let a = precompute(&cfg);
        let b = precompute(&cfg);
        assert_eq!(a, b);
        assert_eq!(a.len(), 7);
    }

    #[test]
    fn bitslice_precompute_matches_scalar() {
        let cfg = PrecomputeConfig {
            hash: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
            table_index: 0,
            chain_len: 128,
            device_num: 0,
            total_devices: 1,
            workgroup_size: 64,
            gpu_target_steps_per_dispatch: crate::params::GPU_TARGET_STEPS_PER_DISPATCH,
        };
        let bit = precompute(&cfg);
        let mut scalar = Vec::with_capacity(cfg.output_len() as usize);
        for abs in 0..cfg.output_len() {
            scalar.push(precompute_one(
                &cfg.hash,
                cfg.reduction_offset(),
                cfg.chain_len,
                cfg.device_num,
                cfg.total_devices,
                abs,
                0,
            ));
        }
        assert_eq!(bit, scalar);
    }

    #[test]
    fn cost_balanced_precompute_batches_preserve_output_order() {
        let cfg = PrecomputeConfig {
            hash: [0x25, 0x77, 0x89, 0x87, 0x04, 0x01, 0xc9, 0x65],
            table_index: 0,
            chain_len: 1_026,
            device_num: 0,
            total_devices: 1,
            workgroup_size: 64,
            gpu_target_steps_per_dispatch: crate::params::GPU_TARGET_STEPS_PER_DISPATCH,
        };
        let bit = precompute(&cfg);
        let scalar = (0..cfg.output_len())
            .map(|abs| {
                precompute_one(
                    &cfg.hash,
                    cfg.reduction_offset(),
                    cfg.chain_len,
                    cfg.device_num,
                    cfg.total_devices,
                    abs,
                    0,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(bit, scalar);
    }

    #[test]
    fn password_netntlmv1_vector() {
        let pt1 = hex::decode("8846f7eaee8fb1").unwrap().try_into().unwrap();
        let pt2 = hex::decode("17ad06bdd830b7").unwrap().try_into().unwrap();
        let pt3 = hex::decode("586c0000000000").unwrap().try_into().unwrap();

        assert_eq!(hex::encode(expand_des_key(&pt1)), "8923bdfdaf753f63");
        assert_eq!(hex::encode(expand_des_key(&pt2)), "17d741d7ddc1c36f");
        assert_eq!(hex::encode(expand_des_key(&pt3)), "5937010101010101");
        assert_eq!(hex::encode(netntlmv1_hash(&pt1)), "727b4e35f947129e");
        assert_eq!(hex::encode(netntlmv1_hash(&pt2)), "a52b9cdedae86934");
        assert_eq!(hex::encode(netntlmv1_hash(&pt3)), "bb23ef89f50fc595");
        assert!(is_exact_des_key_match(
            0x0088_46f7_eaee_8fb1,
            &netntlmv1_hash(&pt1)
        ));
        assert_eq!(recover_pt3(&netntlmv1_hash(&pt3)), Some([0x58, 0x6c]));
        assert_eq!(
            hex::encode(assemble_nt_hash(
                0x0088_46f7_eaee_8fb1,
                0x0017_ad06_bdd8_30b7,
                [0x58, 0x6c]
            )),
            "8846f7eaee8fb117ad06bdd830b7586c"
        );
    }

    #[test]
    fn password2_exact_des_regression() {
        let des1: [u8; 8] = hex::decode("257789870401c965").unwrap().try_into().unwrap();
        assert!(is_exact_des_key_match(0x00e2_2e04_519a_a757, &des1));
    }
}
