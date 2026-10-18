//! Checksums (feature 47). CRC-32 (IEEE 802.3, the zlib polynomial) for integrity of stored data, and a
//! 64-bit hash for hash tables and fingerprints. The CRC table is computed at compile time.
//!
//! The 64-bit hash is not cryptographic. Use `qs_crypt` for anything that must resist an adversary.

const POLY: u32 = 0xEDB8_8320;

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { POLY ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = build_table();

/// CRC-32 of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0, data)
}

/// Continues a CRC-32 computation: `crc32_update(crc32(a), b) == crc32(a ++ b)`.
pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let mut c = !crc;
    for &b in data {
        c = CRC_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

const K0: u64 = 0xa076_1d64_78bd_642f;
const K1: u64 = 0xe703_7ed1_a0b4_28db;
const K2: u64 = 0x8ebc_6af0_9c88_c6e3;
const K3: u64 = 0x5899_65cc_7537_4cc3;

fn mix(a: u64, b: u64) -> u64 {
    let r = (a as u128) * (b as u128);
    (r as u64) ^ ((r >> 64) as u64)
}

/// A seeded 64-bit hash of `data`.
pub fn hash64(data: &[u8], seed: u64) -> u64 {
    let mut h = seed ^ K0;
    let mut chunks = data.chunks_exact(8);
    for chunk in &mut chunks {
        let mut word = [0u8; 8];
        word.copy_from_slice(chunk);
        h = mix(h ^ u64::from_le_bytes(word), K1);
    }
    let rest = chunks.remainder();
    let mut tail = [0u8; 8];
    tail[..rest.len()].copy_from_slice(rest);
    h = mix(h ^ u64::from_le_bytes(tail) ^ (data.len() as u64), K2);
    mix(h, K3)
}
