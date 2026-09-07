//! Portable 64-lane bitsliced DES (derived from fast-des, MIT OR Apache-2.0).

#![allow(clippy::all)]

const IP: [u64; 64] = [
    58, 50, 42, 34, 26, 18, 10, 2, 60, 52, 44, 36, 28, 20, 12, 4, 62, 54, 46, 38, 30, 22, 14, 6,
    64, 56, 48, 40, 32, 24, 16, 8, 57, 49, 41, 33, 25, 17, 9, 1, 59, 51, 43, 35, 27, 19, 11, 3, 61,
    53, 45, 37, 29, 21, 13, 5, 63, 55, 47, 39, 31, 23, 15, 7,
];

const IP_INVO: [usize; 64] = [
    39, 7, 47, 15, 55, 23, 63, 31, 38, 6, 46, 14, 54, 22, 62, 30, 37, 5, 45, 13, 53, 21, 61, 29,
    36, 4, 44, 12, 52, 20, 60, 28, 35, 3, 43, 11, 51, 19, 59, 27, 34, 2, 42, 10, 50, 18, 58, 26,
    33, 1, 41, 9, 49, 17, 57, 25, 32, 0, 40, 8, 48, 16, 56, 24,
];

const SUBKEY_SCHEDULE: [[usize; 48]; 16] = [
    //0:
    [
        9, 50, 33, 59, 48, 16, 32, 56, 1, 8, 18, 41, 2, 34, 25, 24, 43, 57, 58, 0, 35, 26, 17, 40,
        21, 27, 38, 53, 36, 3, 46, 29, 4, 52, 22, 28, 60, 20, 37, 62, 14, 19, 44, 13, 12, 61, 54,
        30,
    ],
    //1:
    [
        1, 42, 25, 51, 40, 8, 24, 48, 58, 0, 10, 33, 59, 26, 17, 16, 35, 49, 50, 57, 56, 18, 9, 32,
        13, 19, 30, 45, 28, 62, 38, 21, 27, 44, 14, 20, 52, 12, 29, 54, 6, 11, 36, 5, 4, 53, 46,
        22,
    ],
    //2:
    [
        50, 26, 9, 35, 24, 57, 8, 32, 42, 49, 59, 17, 43, 10, 1, 0, 48, 33, 34, 41, 40, 2, 58, 16,
        60, 3, 14, 29, 12, 46, 22, 5, 11, 28, 61, 4, 36, 27, 13, 38, 53, 62, 20, 52, 19, 37, 30, 6,
    ],
    //3:
    [
        34, 10, 58, 48, 8, 41, 57, 16, 26, 33, 43, 1, 56, 59, 50, 49, 32, 17, 18, 25, 24, 51, 42,
        0, 44, 54, 61, 13, 27, 30, 6, 52, 62, 12, 45, 19, 20, 11, 60, 22, 37, 46, 4, 36, 3, 21, 14,
        53,
    ],
    //4:
    [
        18, 59, 42, 32, 57, 25, 41, 0, 10, 17, 56, 50, 40, 43, 34, 33, 16, 1, 2, 9, 8, 35, 26, 49,
        28, 38, 45, 60, 11, 14, 53, 36, 46, 27, 29, 3, 4, 62, 44, 6, 21, 30, 19, 20, 54, 5, 61, 37,
    ],
    //5:
    [
        2, 43, 26, 16, 41, 9, 25, 49, 59, 1, 40, 34, 24, 56, 18, 17, 0, 50, 51, 58, 57, 48, 10, 33,
        12, 22, 29, 44, 62, 61, 37, 20, 30, 11, 13, 54, 19, 46, 28, 53, 5, 14, 3, 4, 38, 52, 45,
        21,
    ],
    //6:
    [
        51, 56, 10, 0, 25, 58, 9, 33, 43, 50, 24, 18, 8, 40, 2, 1, 49, 34, 35, 42, 41, 32, 59, 17,
        27, 6, 13, 28, 46, 45, 21, 4, 14, 62, 60, 38, 3, 30, 12, 37, 52, 61, 54, 19, 22, 36, 29, 5,
    ],
    //7:
    [
        35, 40, 59, 49, 9, 42, 58, 17, 56, 34, 8, 2, 57, 24, 51, 50, 33, 18, 48, 26, 25, 16, 43, 1,
        11, 53, 60, 12, 30, 29, 5, 19, 61, 46, 44, 22, 54, 14, 27, 21, 36, 45, 38, 3, 6, 20, 13,
        52,
    ],
    //8:
    [
        56, 32, 51, 41, 1, 34, 50, 9, 48, 26, 0, 59, 49, 16, 43, 42, 25, 10, 40, 18, 17, 8, 35, 58,
        3, 45, 52, 4, 22, 21, 60, 11, 53, 38, 36, 14, 46, 6, 19, 13, 28, 37, 30, 62, 61, 12, 5, 44,
    ],
    //9:
    [
        40, 16, 35, 25, 50, 18, 34, 58, 32, 10, 49, 43, 33, 0, 56, 26, 9, 59, 24, 2, 1, 57, 48, 42,
        54, 29, 36, 19, 6, 5, 44, 62, 37, 22, 20, 61, 30, 53, 3, 60, 12, 21, 14, 46, 45, 27, 52,
        28,
    ],
    //10:
    [
        24, 0, 48, 9, 34, 2, 18, 42, 16, 59, 33, 56, 17, 49, 40, 10, 58, 43, 8, 51, 50, 41, 32, 26,
        38, 13, 20, 3, 53, 52, 28, 46, 21, 6, 4, 45, 14, 37, 54, 44, 27, 5, 61, 30, 29, 11, 36, 12,
    ],
    //11:
    [
        8, 49, 32, 58, 18, 51, 2, 26, 0, 43, 17, 40, 1, 33, 24, 59, 42, 56, 57, 35, 34, 25, 16, 10,
        22, 60, 4, 54, 37, 36, 12, 30, 5, 53, 19, 29, 61, 21, 38, 28, 11, 52, 45, 14, 13, 62, 20,
        27,
    ],
    //12:
    [
        57, 33, 16, 42, 2, 35, 51, 10, 49, 56, 1, 24, 50, 17, 8, 43, 26, 40, 41, 48, 18, 9, 0, 59,
        6, 44, 19, 38, 21, 20, 27, 14, 52, 37, 3, 13, 45, 5, 22, 12, 62, 36, 29, 61, 60, 46, 4, 11,
    ],
    //13:
    [
        41, 17, 0, 26, 51, 48, 35, 59, 33, 40, 50, 8, 34, 1, 57, 56, 10, 24, 25, 32, 2, 58, 49, 43,
        53, 28, 3, 22, 5, 4, 11, 61, 36, 21, 54, 60, 29, 52, 6, 27, 46, 20, 13, 45, 44, 30, 19, 62,
    ],
    //14:
    [
        25, 1, 49, 10, 35, 32, 48, 43, 17, 24, 34, 57, 18, 50, 41, 40, 59, 8, 9, 16, 51, 42, 33,
        56, 37, 12, 54, 6, 52, 19, 62, 45, 20, 5, 38, 44, 13, 36, 53, 11, 30, 4, 60, 29, 28, 14, 3,
        46,
    ],
    //15:
    [
        17, 58, 41, 2, 56, 24, 40, 35, 9, 16, 26, 49, 10, 42, 33, 32, 51, 0, 1, 8, 43, 34, 25, 48,
        29, 4, 46, 61, 44, 11, 54, 37, 12, 60, 30, 36, 5, 28, 45, 3, 22, 27, 52, 21, 20, 6, 62, 38,
    ],
];

/// Scalar Eklundh bit-matrix transpose (from bitsliced-op, MIT OR Apache-2.0).

pub fn transpose_64x64(input: &[u64; 64]) -> [u64; 64] {
    let mut out = *input;

    let mut j = 32;

    let mut mask: u64 = 0x00000000FFFFFFFF;

    for _ in 0..6 {
        for k in 0..64 {
            if (k & j) == 0 {
                let x = out[k];

                let y = out[k | j];

                let t = (x ^ (y >> j)) & mask;

                out[k] = x ^ t;

                out[k | j] = y ^ (t << j);
            }
        }

        j >>= 1;

        mask ^= mask << j;
    }

    out
}

fn permute_bits_pc(p_box: &[u64], input: u64, input_length: usize) -> u64 {
    let mut output: u64 = 0;

    for (i, &src) in p_box.iter().enumerate() {
        let bit = (input >> (input_length - src as usize)) & 1;

        output |= bit << (p_box.len() - (i + 1));
    }

    output
}

fn convert_to_key(plaintexts: &[u64; 64], all_ones: u64) -> [u64; 64] {
    [
        plaintexts[8],
        plaintexts[9],
        plaintexts[10],
        plaintexts[11],
        plaintexts[12],
        plaintexts[13],
        plaintexts[14],
        all_ones,
        plaintexts[15],
        plaintexts[16],
        plaintexts[17],
        plaintexts[18],
        plaintexts[19],
        plaintexts[20],
        plaintexts[21],
        all_ones,
        plaintexts[22],
        plaintexts[23],
        plaintexts[24],
        plaintexts[25],
        plaintexts[26],
        plaintexts[27],
        plaintexts[28],
        all_ones,
        plaintexts[29],
        plaintexts[30],
        plaintexts[31],
        plaintexts[32],
        plaintexts[33],
        plaintexts[34],
        plaintexts[35],
        all_ones,
        plaintexts[36],
        plaintexts[37],
        plaintexts[38],
        plaintexts[39],
        plaintexts[40],
        plaintexts[41],
        plaintexts[42],
        all_ones,
        plaintexts[43],
        plaintexts[44],
        plaintexts[45],
        plaintexts[46],
        plaintexts[47],
        plaintexts[48],
        plaintexts[49],
        all_ones,
        plaintexts[50],
        plaintexts[51],
        plaintexts[52],
        plaintexts[53],
        plaintexts[54],
        plaintexts[55],
        plaintexts[56],
        all_ones,
        plaintexts[57],
        plaintexts[58],
        plaintexts[59],
        plaintexts[60],
        plaintexts[61],
        plaintexts[62],
        plaintexts[63],
        all_ones,
    ]
}

#[inline(always)]

fn feistel(l: &mut [u64; 32], r: &[u64; 32], keys: &[u64; 64], round: usize) {
    use crate::bs_sboxes::{s1, s2, s3, s4, s5, s6, s7, s8};

    let mut e0 = r[31] ^ keys[SUBKEY_SCHEDULE[round][0]];

    let mut e1 = r[0] ^ keys[SUBKEY_SCHEDULE[round][1]];

    let mut e2 = r[1] ^ keys[SUBKEY_SCHEDULE[round][2]];

    let mut e3 = r[2] ^ keys[SUBKEY_SCHEDULE[round][3]];

    let mut e4 = r[3] ^ keys[SUBKEY_SCHEDULE[round][4]];

    let mut e5 = r[4] ^ keys[SUBKEY_SCHEDULE[round][5]];

    let mut f0 = r[3] ^ keys[SUBKEY_SCHEDULE[round][6]];

    let mut f1 = r[4] ^ keys[SUBKEY_SCHEDULE[round][7]];

    let mut f2 = r[5] ^ keys[SUBKEY_SCHEDULE[round][8]];

    let mut f3 = r[6] ^ keys[SUBKEY_SCHEDULE[round][9]];

    let mut f4 = r[7] ^ keys[SUBKEY_SCHEDULE[round][10]];

    let mut f5 = r[8] ^ keys[SUBKEY_SCHEDULE[round][11]];

    let mut g0 = r[7] ^ keys[SUBKEY_SCHEDULE[round][12]];

    let mut g1 = r[8] ^ keys[SUBKEY_SCHEDULE[round][13]];

    let mut g2 = r[9] ^ keys[SUBKEY_SCHEDULE[round][14]];

    let mut g3 = r[10] ^ keys[SUBKEY_SCHEDULE[round][15]];

    let mut g4 = r[11] ^ keys[SUBKEY_SCHEDULE[round][16]];

    let mut g5 = r[12] ^ keys[SUBKEY_SCHEDULE[round][17]];

    let mut h0 = r[11] ^ keys[SUBKEY_SCHEDULE[round][18]];

    let mut h1 = r[12] ^ keys[SUBKEY_SCHEDULE[round][19]];

    let mut h2 = r[13] ^ keys[SUBKEY_SCHEDULE[round][20]];

    let mut h3 = r[14] ^ keys[SUBKEY_SCHEDULE[round][21]];

    let mut h4 = r[15] ^ keys[SUBKEY_SCHEDULE[round][22]];

    let mut h5 = r[16] ^ keys[SUBKEY_SCHEDULE[round][23]];

    {
        let (o0, o1, o2, o3) = s1(e0, e1, e2, e3, e4, e5);

        l[8] ^= o0;
        l[16] ^= o1;
        l[22] ^= o2;
        l[30] ^= o3;
    }

    {
        let (o0, o1, o2, o3) = s2(f0, f1, f2, f3, f4, f5);

        l[12] ^= o0;
        l[27] ^= o1;
        l[1] ^= o2;
        l[17] ^= o3;
    }

    {
        let (o0, o1, o2, o3) = s3(g0, g1, g2, g3, g4, g5);

        l[23] ^= o0;
        l[15] ^= o1;
        l[29] ^= o2;
        l[5] ^= o3;
    }

    {
        let (o0, o1, o2, o3) = s4(h0, h1, h2, h3, h4, h5);

        l[25] ^= o0;
        l[19] ^= o1;
        l[9] ^= o2;
        l[0] ^= o3;
    }

    e0 = r[15] ^ keys[SUBKEY_SCHEDULE[round][24]];

    e1 = r[16] ^ keys[SUBKEY_SCHEDULE[round][25]];

    e2 = r[17] ^ keys[SUBKEY_SCHEDULE[round][26]];

    e3 = r[18] ^ keys[SUBKEY_SCHEDULE[round][27]];

    e4 = r[19] ^ keys[SUBKEY_SCHEDULE[round][28]];

    e5 = r[20] ^ keys[SUBKEY_SCHEDULE[round][29]];

    f0 = r[19] ^ keys[SUBKEY_SCHEDULE[round][30]];

    f1 = r[20] ^ keys[SUBKEY_SCHEDULE[round][31]];

    f2 = r[21] ^ keys[SUBKEY_SCHEDULE[round][32]];

    f3 = r[22] ^ keys[SUBKEY_SCHEDULE[round][33]];

    f4 = r[23] ^ keys[SUBKEY_SCHEDULE[round][34]];

    f5 = r[24] ^ keys[SUBKEY_SCHEDULE[round][35]];

    g0 = r[23] ^ keys[SUBKEY_SCHEDULE[round][36]];

    g1 = r[24] ^ keys[SUBKEY_SCHEDULE[round][37]];

    g2 = r[25] ^ keys[SUBKEY_SCHEDULE[round][38]];

    g3 = r[26] ^ keys[SUBKEY_SCHEDULE[round][39]];

    g4 = r[27] ^ keys[SUBKEY_SCHEDULE[round][40]];

    g5 = r[28] ^ keys[SUBKEY_SCHEDULE[round][41]];

    h0 = r[27] ^ keys[SUBKEY_SCHEDULE[round][42]];

    h1 = r[28] ^ keys[SUBKEY_SCHEDULE[round][43]];

    h2 = r[29] ^ keys[SUBKEY_SCHEDULE[round][44]];

    h3 = r[30] ^ keys[SUBKEY_SCHEDULE[round][45]];

    h4 = r[31] ^ keys[SUBKEY_SCHEDULE[round][46]];

    h5 = r[0] ^ keys[SUBKEY_SCHEDULE[round][47]];

    {
        let (o0, o1, o2, o3) = s5(e0, e1, e2, e3, e4, e5);

        l[7] ^= o0;
        l[13] ^= o1;
        l[24] ^= o2;
        l[2] ^= o3;
    }

    {
        let (o0, o1, o2, o3) = s6(f0, f1, f2, f3, f4, f5);

        l[3] ^= o0;
        l[28] ^= o1;
        l[10] ^= o2;
        l[18] ^= o3;
    }

    {
        let (o0, o1, o2, o3) = s7(g0, g1, g2, g3, g4, g5);

        l[31] ^= o0;
        l[11] ^= o1;
        l[21] ^= o2;
        l[6] ^= o3;
    }

    {
        let (o0, o1, o2, o3) = s8(h0, h1, h2, h3, h4, h5);

        l[4] ^= o0;
        l[26] ^= o1;
        l[14] ^= o2;
        l[20] ^= o3;
    }
}

/// Encrypt in-place: keys is bit-sliced key schedule state (64 bit-planes x 64 lanes).

/// After convert_to_key, bits 0..63 hold the expanded DES key material layout used by fast-des.

pub fn encrypt_bitslice(plaintext: u64, keys: &mut [u64; 64]) {
    let ip = permute_bits_pc(&IP, plaintext, 64);

    let mut l = [0u64; 32];

    let mut r = [0u64; 32];

    for i in 0..32 {
        let bit_l = (ip >> (63 - i)) & 1;

        let bit_r = (ip >> (31 - i)) & 1;

        l[i] = 0u64.wrapping_sub(bit_l);

        r[i] = 0u64.wrapping_sub(bit_r);
    }

    feistel(&mut l, &r, keys, 0);

    feistel(&mut r, &l, keys, 1);

    feistel(&mut l, &r, keys, 2);

    feistel(&mut r, &l, keys, 3);

    feistel(&mut l, &r, keys, 4);

    feistel(&mut r, &l, keys, 5);

    feistel(&mut l, &r, keys, 6);

    feistel(&mut r, &l, keys, 7);

    feistel(&mut l, &r, keys, 8);

    feistel(&mut r, &l, keys, 9);

    feistel(&mut l, &r, keys, 10);

    feistel(&mut r, &l, keys, 11);

    feistel(&mut l, &r, keys, 12);

    feistel(&mut r, &l, keys, 13);

    feistel(&mut l, &r, keys, 14);

    feistel(&mut r, &l, keys, 15);

    for i in 0..64 {
        keys[i] = if IP_INVO[i] < 32 {
            r[IP_INVO[i]]
        } else {
            l[IP_INVO[i] - 32]
        };
    }
}

/// NetNTLMv1: 56-bit keys in keys_in (MSB 8 bits ignored), returns ciphertexts as BE integers.

pub fn netntlmv1_64(plaintext: u64, keys_in: &[u64; 64]) -> [u64; 64] {
    let transposed = transpose_64x64(keys_in);

    let mut keys = convert_to_key(&transposed, !0u64);

    encrypt_bitslice(plaintext, &mut keys);

    transpose_64x64(&keys)
}
