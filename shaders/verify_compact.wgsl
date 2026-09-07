// ntlmrain - outflank.nl
// auto-generated shader. do not edit by hand.

struct FalseParams {
    target_lo: u32,
    target_hi: u32,
    reduction_offset: u32,
    candidate_count: u32,
    step_budget: u32,
    _padding0: u32,
    _padding1: u32,
    _padding2: u32,
}

struct CandidateState {
    index: vec2<u32>,
    result: vec2<u32>,
    target_position: u32,
    next_position: u32,
    found: u32,
    _padding: u32,
}

@group(0) @binding(0) var<uniform> params: FalseParams;
@group(0) @binding(1) var<storage, read_write> state_buf: array<CandidateState>;
@group(0) @binding(2) var<storage, read> lut_source: array<u32>;
@group(0) @binding(3) var<storage, read_write> completion_buf: array<u32>;

override WORKGROUP_SIZE: u32 = 64u;
const COMPLETION_MAGIC: u32 = 0x42593731u;
const COMPLETION_FOUND_MAGIC: u32 = 0x42593732u;
const KEY_SOURCE_BASE: u32 = 544u;
const PC1_SOURCE_BASE: u32 = 2592u;
const PAIR_SOURCE_BASE: u32 = 4000u;
const PAIR_CODE_SOURCE_BASE: u32 = 20384u;
const PAIR_EXPAND_SOURCE_BASE: u32 = 24480u;

var<workgroup> lut_cache: array<u32, 544>;
var<workgroup> completion_count: atomic<u32>;
var<workgroup> found_in_group: atomic<u32>;

struct Pair {
    lo: u32,
    hi: u32,
}

fn lut_at_scalar(base: u32, index: u32) -> u32 { return lut_cache[base + index]; }

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

fn des_pc1_from_index(index: Pair) -> Pair {
    let p0 = (index.hi >> 16u) & 0xffu;
    let p1 = (index.hi >> 8u) & 0xffu;
    let p2 = index.hi & 0xffu;
    let p3 = (index.lo >> 24u) & 0xffu;
    let p4 = (index.lo >> 16u) & 0xffu;
    let p5 = (index.lo >> 8u) & 0xffu;
    let p6 = index.lo & 0xffu;

    let k0 = p0 & 0xfeu;
    let k1 = (((p0 & 0x01u) << 6u) | ((p1 >> 2u) & 0x3fu)) << 1u;
    let k2 = (((p1 & 0x03u) << 5u) | ((p2 >> 3u) & 0x1fu)) << 1u;
    let k3 = (((p2 & 0x07u) << 4u) | ((p3 >> 4u) & 0x0fu)) << 1u;
    let k4 = (((p3 & 0x0fu) << 3u) | ((p4 >> 5u) & 0x07u)) << 1u;
    let k5 = (((p4 & 0x1fu) << 2u) | ((p5 >> 6u) & 0x03u)) << 1u;
    let k6 = (((p5 & 0x3fu) << 1u) | ((p6 >> 7u) & 0x01u)) << 1u;
    let k7 = (p6 & 0x7fu) << 1u;

    var x = (k0 << 24u) | (k1 << 16u) | (k2 << 8u) | k3;
    var y = (k4 << 24u) | (k5 << 16u) | (k6 << 8u) | k7;
    var temp = ((y >> 4u) ^ x) & 0x0f0f0f0fu;
    x ^= temp;
    y ^= temp << 4u;
    temp = (y ^ x) & 0x10101010u;
    x ^= temp;
    y ^= temp;

    x = (lut_at_scalar(512u, x & 0x0fu) << 3u)
        | (lut_at_scalar(512u, (x >> 8u) & 0x0fu) << 2u)
        | (lut_at_scalar(512u, (x >> 16u) & 0x0fu) << 1u)
        | lut_at_scalar(512u, (x >> 24u) & 0x0fu)
        | (lut_at_scalar(512u, (x >> 5u) & 0x0fu) << 7u)
        | (lut_at_scalar(512u, (x >> 13u) & 0x0fu) << 6u)
        | (lut_at_scalar(512u, (x >> 21u) & 0x0fu) << 5u)
        | (lut_at_scalar(512u, (x >> 29u) & 0x0fu) << 4u);

    y = (lut_at_scalar(528u, (y >> 1u) & 0x0fu) << 3u)
        | (lut_at_scalar(528u, (y >> 9u) & 0x0fu) << 2u)
        | (lut_at_scalar(528u, (y >> 17u) & 0x0fu) << 1u)
        | lut_at_scalar(528u, (y >> 25u) & 0x0fu)
        | (lut_at_scalar(528u, (y >> 4u) & 0x0fu) << 7u)
        | (lut_at_scalar(528u, (y >> 12u) & 0x0fu) << 6u)
        | (lut_at_scalar(528u, (y >> 20u) & 0x0fu) << 5u)
        | (lut_at_scalar(528u, (y >> 28u) & 0x0fu) << 4u);

    return Pair(x & 0x0fffffffu, y & 0x0fffffffu);
}

fn des_subkeys(x: u32, y: u32) -> Pair {
    let a = ((x << 4u) & 0x24000000u)
        | ((x << 28u) & 0x10000000u)
        | ((x << 14u) & 0x08000000u)
        | ((x << 18u) & 0x02080000u)
        | ((x << 6u) & 0x01000000u)
        | ((x << 9u) & 0x00200000u)
        | ((x >> 1u) & 0x00100000u)
        | ((x << 10u) & 0x00040000u)
        | ((x << 2u) & 0x00020000u)
        | ((x >> 10u) & 0x00010000u)
        | ((y >> 13u) & 0x00002000u)
        | ((y >> 4u) & 0x00001000u)
        | ((y << 6u) & 0x00000800u)
        | ((y >> 1u) & 0x00000400u)
        | ((y >> 14u) & 0x00000200u)
        | (y & 0x00000100u)
        | ((y >> 5u) & 0x00000020u)
        | ((y >> 10u) & 0x00000010u)
        | ((y >> 3u) & 0x00000008u)
        | ((y >> 18u) & 0x00000004u)
        | ((y >> 26u) & 0x00000002u)
        | ((y >> 24u) & 0x00000001u);

    let b = ((x << 15u) & 0x20000000u)
        | ((x << 17u) & 0x10000000u)
        | ((x << 10u) & 0x08000000u)
        | ((x << 22u) & 0x04000000u)
        | ((x >> 2u) & 0x02000000u)
        | ((x << 1u) & 0x01000000u)
        | ((x << 16u) & 0x00200000u)
        | ((x << 11u) & 0x00100000u)
        | ((x << 3u) & 0x00080000u)
        | ((x >> 6u) & 0x00040000u)
        | ((x << 15u) & 0x00020000u)
        | ((x >> 4u) & 0x00010000u)
        | ((y >> 2u) & 0x00002000u)
        | ((y << 8u) & 0x00001000u)
        | ((y >> 14u) & 0x00000808u)
        | ((y >> 9u) & 0x00000400u)
        | (y & 0x00000200u)
        | ((y << 7u) & 0x00000100u)
        | ((y >> 7u) & 0x00000020u)
        | ((y >> 3u) & 0x00000011u)
        | ((y << 2u) & 0x00000004u)
        | ((y >> 21u) & 0x00000002u);
    return Pair(a, b);
}

fn sbox_at(base: u32, index: u32) -> u32 {
    return lut_cache[base + index];
}

fn des_f(kx: u32, ky: u32, r: u32) -> u32 {
    let subkeys = des_subkeys(kx, ky);
    let a = subkeys.lo ^ r;
    var result = sbox_at(448u, extractBits(a, 0u, 6u))
        ^ sbox_at(320u, extractBits(a, 8u, 6u))
        ^ sbox_at(192u, extractBits(a, 16u, 6u))
        ^ sbox_at(64u, extractBits(a, 24u, 6u));
    let b = subkeys.hi ^ ((r << 28u) | (r >> 4u));
    result ^= sbox_at(384u, extractBits(b, 0u, 6u))
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
    var load_index = local_id;
    while (load_index < 544u) {
        lut_cache[load_index] = lut_source[load_index];
        load_index += WORKGROUP_SIZE;
    }
    if (local_id == 0u) {
        atomicStore(&completion_count, 0u);
        atomicStore(&found_in_group, 0u);
    }
    workgroupBarrier();

    let gid = gid3.x;
    if (gid < params.candidate_count) {
        var state = state_buf[gid];
        if (state.found != 0u) {
            atomicStore(&found_in_group, 1u);
        }

        var steps = 0u;
        loop {
            if (
                state.found != 0u
                || state.next_position > state.target_position
                || steps >= params.step_budget
            ) {
                break;
            }

            let previous = Pair(state.index.x, state.index.y);
            let hash = netntlmv1_hash_index(previous);
            let next = mask56(add_pair_u32(
                add_pair_u32(hash, params.reduction_offset),
                state.next_position));
            state.index = vec2<u32>(next.lo, next.hi);
            // Reduction intentionally keeps only 56 bits for table walking,
            // but a recovered DES key must match the full 64-bit ciphertext.
            if (hash.lo == params.target_lo && hash.hi == params.target_hi) {
                state.result = vec2<u32>(previous.lo, previous.hi);
                state.found = 1u;
                atomicStore(&found_in_group, 1u);
            }
            state.next_position += 1u;
            steps += 1u;
        }
        state_buf[gid] = state;
    }

    let completed_before = atomicAdd(&completion_count, 1u);
    if (completed_before + 1u == WORKGROUP_SIZE) {
        completion_buf[workgroup_id.x] = select(
            COMPLETION_MAGIC,
            COMPLETION_FOUND_MAGIC,
            atomicLoad(&found_in_group) != 0u);
    }
}
