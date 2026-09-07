// ntlmrain - outflank.nl
// auto-generated shader. do not edit by hand.

struct Params {
    hash_lo: u32,
    hash_hi: u32,
    reduction_offset: u32,
    chain_len: u32,
    endpoint_start: u32,
    slice_start: u32,
    slice_steps: u32,
    output_len: u32,
    benchmark_steps: u32,
    mode: u32,
    _padding0: u32,
    _padding1: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read_write> output_buf: array<vec2<u32>>;
@group(0) @binding(2) var<storage, read> lut_source: array<u32>;
@group(0) @binding(3) var<storage, read_write> completion_buf: array<u32>;

override WORKGROUP_SIZE: u32 = 64u;
const COMPLETION_MAGIC: u32 = 0x42593731u;
const KEY_SOURCE_BASE: u32 = 544u;
const PC1_SOURCE_BASE: u32 = 2592u;
const PAIR_SOURCE_BASE: u32 = 4000u;
const PAIR_CODE_SOURCE_BASE: u32 = 20384u;
const PAIR_EXPAND_SOURCE_BASE: u32 = 24480u;

var<workgroup> sbox_cache: array<u32, 384>;
var<workgroup> pair_cache0: array<u32, 4096>;
var<workgroup> key_cache: array<vec2<u32>, 1024>;
var<workgroup> pc1_cache: array<vec2<u32>, 704>;
var<workgroup> completion_count: atomic<u32>;

struct Pair {
    lo: u32,
    hi: u32,
}


fn add_pair_u32(a: Pair, b: u32) -> Pair {
    let lo = a.lo + b;
    let carry = select(0u, 1u, lo < a.lo);
    return Pair(lo, a.hi + carry);
}

fn mask56(value: Pair) -> Pair {
    return Pair(value.lo, value.hi & 0x00ffffffu);
}

fn rotate28_1(value: u32) -> u32 {
    return ((value << 1u) | (value >> 27u)) & 0x0fffffffu;
}

fn rotate28_2(value: u32) -> u32 {
    return ((value << 2u) | (value >> 26u)) & 0x0fffffffu;
}

fn bswap32(value: u32) -> u32 {
    return (value >> 24u)
        | ((value >> 8u) & 0x0000ff00u)
        | ((value << 8u) & 0x00ff0000u)
        | (value << 24u);
}

fn des_fp_pair(a_in: u32, b_in: u32) -> Pair {
    var a = (a_in << 31u) | (a_in >> 1u);
    var b = b_in;
    var temp = (a ^ b) & 0xaaaaaaaau;
    a ^= temp;
    b ^= temp;
    b = (b << 31u) | (b >> 1u);
    temp = ((b >> 8u) ^ a) & 0x00ff00ffu;
    a ^= temp;
    b ^= temp << 8u;
    temp = ((b >> 2u) ^ a) & 0x33333333u;
    a ^= temp;
    b ^= temp << 2u;
    temp = ((a >> 16u) ^ b) & 0x0000ffffu;
    b ^= temp;
    a ^= temp << 16u;
    temp = ((a >> 4u) ^ b) & 0x0f0f0f0fu;
    b ^= temp;
    a ^= temp << 4u;
    return Pair(a, b);
}

fn pc1_at(base: u32, index: u32) -> Pair {
    let value = pc1_cache[base + index];
    return Pair(value.x, value.y);
}

fn des_pc1_from_index(index: Pair) -> Pair {
    let e0 = pc1_at(0u, extractBits(index.lo, 0u, 7u));
    let e1 = pc1_at(128u, extractBits(index.lo, 7u, 7u));
    let e2 = pc1_at(256u, extractBits(index.lo, 14u, 6u));
    let e3 = pc1_at(320u, extractBits(index.lo, 20u, 6u));
    let e4 = pc1_at(384u, extractBits(index.lo, 26u, 6u));
    let e5 = pc1_at(448u, extractBits(index.hi, 0u, 6u));
    let e6 = pc1_at(512u, extractBits(index.hi, 6u, 6u));
    let e7 = pc1_at(576u, extractBits(index.hi, 12u, 6u));
    let e8 = pc1_at(640u, extractBits(index.hi, 18u, 6u));
    return Pair(e0.lo | e1.lo | e2.lo | e3.lo | e4.lo | e5.lo | e6.lo | e7.lo | e8.lo, e0.hi | e1.hi | e2.hi | e3.hi | e4.hi | e5.hi | e6.hi | e7.hi | e8.hi);
}

fn key_at(base: u32, index: u32) -> Pair {
    let value = key_cache[base + index];
    return Pair(value.x, value.y);
}

fn des_subkeys(x: u32, y: u32) -> Pair {
    let e0 = key_at(0u, extractBits(x, 0u, 7u));
    let e1 = key_at(128u, extractBits(x, 7u, 7u));
    let e2 = key_at(256u, extractBits(x, 14u, 7u));
    let e3 = key_at(384u, extractBits(x, 21u, 7u));
    let e4 = key_at(512u, extractBits(y, 0u, 7u));
    let e5 = key_at(640u, extractBits(y, 7u, 7u));
    let e6 = key_at(768u, extractBits(y, 14u, 7u));
    let e7 = key_at(896u, extractBits(y, 21u, 7u));
    return Pair(e0.lo | e1.lo | e2.lo | e3.lo | e4.lo | e5.lo | e6.lo | e7.lo, e0.hi | e1.hi | e2.hi | e3.hi | e4.hi | e5.hi | e6.hi | e7.hi);
}

fn sbox_at(base: u32, index: u32) -> u32 {
    return sbox_cache[base + index];
}

fn pair_at_0(index: u32) -> u32 {
    return pair_cache0[index];
}

fn des_f(kx: u32, ky: u32, r: u32) -> u32 {
    let subkeys = des_subkeys(kx, ky);
    let a = subkeys.lo ^ r;
    var result = pair_at_0(extractBits(a, 0u, 6u) | (extractBits(a, 8u, 6u) << 6u))
        ^ sbox_at(192u, extractBits(a, 16u, 6u))
        ^ sbox_at(64u, extractBits(a, 24u, 6u));
    let b = subkeys.hi ^ ((r << 28u) | (r >> 4u));
    result ^= sbox_at(320u, extractBits(b, 0u, 6u))
        ^ sbox_at(256u, extractBits(b, 8u, 6u))
        ^ sbox_at(128u, extractBits(b, 16u, 6u))
        ^ sbox_at(0u, extractBits(b, 24u, 6u));
    return result;
}

fn netntlmv1_hash_index(index: Pair) -> Pair {
    let key = des_pc1_from_index(index);
    var kx = key.lo;
    var ky = key.hi;
    var x = 0xf0aaf0aau;
    var y = 0x00cd00cdu;
    kx = rotate28_1(kx); ky = rotate28_1(ky); x ^= des_f(kx, ky, y);
    kx = rotate28_1(kx); ky = rotate28_1(ky); y ^= des_f(kx, ky, x);
    kx = rotate28_2(kx); ky = rotate28_2(ky); x ^= des_f(kx, ky, y);
    kx = rotate28_2(kx); ky = rotate28_2(ky); y ^= des_f(kx, ky, x);
    kx = rotate28_2(kx); ky = rotate28_2(ky); x ^= des_f(kx, ky, y);
    kx = rotate28_2(kx); ky = rotate28_2(ky); y ^= des_f(kx, ky, x);
    kx = rotate28_2(kx); ky = rotate28_2(ky); x ^= des_f(kx, ky, y);
    kx = rotate28_2(kx); ky = rotate28_2(ky); y ^= des_f(kx, ky, x);
    kx = rotate28_1(kx); ky = rotate28_1(ky); x ^= des_f(kx, ky, y);
    kx = rotate28_2(kx); ky = rotate28_2(ky); y ^= des_f(kx, ky, x);
    kx = rotate28_2(kx); ky = rotate28_2(ky); x ^= des_f(kx, ky, y);
    kx = rotate28_2(kx); ky = rotate28_2(ky); y ^= des_f(kx, ky, x);
    kx = rotate28_2(kx); ky = rotate28_2(ky); x ^= des_f(kx, ky, y);
    kx = rotate28_2(kx); ky = rotate28_2(ky); y ^= des_f(kx, ky, x);
    kx = rotate28_2(kx); ky = rotate28_2(ky); x ^= des_f(kx, ky, y);
    kx = rotate28_1(kx); ky = rotate28_1(ky); y ^= des_f(kx, ky, x);
    let fp = des_fp_pair(y, x);
    return Pair(bswap32(fp.lo), bswap32(fp.hi));
}

@compute @workgroup_size(WORKGROUP_SIZE)
fn main(
    @builtin(global_invocation_id) gid3: vec3<u32>,
    @builtin(local_invocation_index) local_id: u32,
    @builtin(workgroup_id) workgroup_id: vec3<u32>,
) {
    var load_sbox_0 = local_id;
    while (load_sbox_0 < 64u) {
        sbox_cache[0u + load_sbox_0] = lut_source[0u + load_sbox_0];
        load_sbox_0 += WORKGROUP_SIZE;
    }
    var load_sbox_1 = local_id;
    while (load_sbox_1 < 64u) {
        sbox_cache[64u + load_sbox_1] = lut_source[64u + load_sbox_1];
        load_sbox_1 += WORKGROUP_SIZE;
    }
    var load_sbox_2 = local_id;
    while (load_sbox_2 < 64u) {
        sbox_cache[128u + load_sbox_2] = lut_source[128u + load_sbox_2];
        load_sbox_2 += WORKGROUP_SIZE;
    }
    var load_sbox_3 = local_id;
    while (load_sbox_3 < 64u) {
        sbox_cache[192u + load_sbox_3] = lut_source[192u + load_sbox_3];
        load_sbox_3 += WORKGROUP_SIZE;
    }
    var load_sbox_4 = local_id;
    while (load_sbox_4 < 64u) {
        sbox_cache[256u + load_sbox_4] = lut_source[256u + load_sbox_4];
        load_sbox_4 += WORKGROUP_SIZE;
    }
    var load_sbox_5 = local_id;
    while (load_sbox_5 < 64u) {
        sbox_cache[320u + load_sbox_5] = lut_source[384u + load_sbox_5];
        load_sbox_5 += WORKGROUP_SIZE;
    }
    var load_pair_0 = local_id;
    while (load_pair_0 < 4096u) {
        pair_cache0[load_pair_0] = lut_source[PAIR_SOURCE_BASE + 0u + load_pair_0];
        load_pair_0 += WORKGROUP_SIZE;
    }
    var load_key = local_id;
    while (load_key < 1024u) {
        let source = KEY_SOURCE_BASE + load_key * 2u;
        key_cache[load_key] = vec2<u32>(lut_source[source], lut_source[source + 1u]);
        load_key += WORKGROUP_SIZE;
    }

    var load_pc1 = local_id;
    while (load_pc1 < 704u) {
        let source = PC1_SOURCE_BASE + load_pc1 * 2u;
        pc1_cache[load_pc1] = vec2<u32>(lut_source[source], lut_source[source + 1u]);
        load_pc1 += WORKGROUP_SIZE;
    }
    if (local_id == 0u) {
        atomicStore(&completion_count, 0u);
    }
    workgroupBarrier();

    let benchmark_mode = params.mode == 1u;
    let gid = gid3.x;
    let valid = gid < params.output_len;
    let global_gid = params.endpoint_start + gid;
    let total_steps = select(global_gid, params.benchmark_steps, benchmark_mode);
    let checkpoint_start = select(params.slice_start, 0u, benchmark_mode);
    let checkpoint_size = select(params.slice_steps, total_steps, benchmark_mode);
    let available_steps = select(0u, total_steps - checkpoint_start, total_steps > checkpoint_start);
    var remaining = select(0u, min(checkpoint_size, available_steps), valid);
    let target_pos = params.chain_len - 1u - total_steps;
    let seed_delta = select(target_pos - 1u, target_pos - 1u + gid, benchmark_mode);
    var index = Pair(0u, 0u);
    if (valid) {
        if (benchmark_mode || checkpoint_start == 0u) {
            index = mask56(add_pair_u32(
                add_pair_u32(Pair(params.hash_lo, params.hash_hi), params.reduction_offset),
                seed_delta));
        } else {
            let saved = output_buf[gid];
            index = Pair(saved.x, saved.y);
        }
    }
    var position = target_pos + checkpoint_start;

    loop {
        if (remaining == 0u) { break; }
        let hash = netntlmv1_hash_index(index);
        index = mask56(add_pair_u32(
            add_pair_u32(hash, params.reduction_offset),
            position));
        position += 1u;
        remaining -= 1u;
    }
    if (valid) {
        output_buf[gid] = vec2<u32>(index.lo, index.hi);
    }

    // The last invocation to finish publishes the completion marker.
    // Dispatch-edge invocations can execute different chain lengths, so a
    // control barrier here would be reached through non-uniform control flow.
    let completed_before = atomicAdd(&completion_count, 1u);
    if (completed_before + 1u == WORKGROUP_SIZE) {
        completion_buf[workgroup_id.x] = COMPLETION_MAGIC;
    }
}
