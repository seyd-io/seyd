//! GF(256) arithmetic, field polynomial 0x11d.
//!
//! Every constant here is part of the wire contract: the pilot's TypeScript
//! implementation and the legacy Python reference (`packages/agent/fec.py`)
//! derive the same tables from the same polynomial, and
//! `tools/fec-vectors.py` exists to prove it byte for byte.

use std::sync::LazyLock;

pub const POLY: u16 = 0x11d;

const fn build_tables() -> ([u8; 512], [u8; 256]) {
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];
    let mut x: u16 = 1;
    let mut i = 0;
    while i < 255 {
        exp[i] = x as u8;
        log[x as usize] = i as u8;
        x <<= 1;
        if x & 0x100 != 0 {
            x ^= POLY;
        }
        i += 1;
    }
    let mut i = 255;
    while i < 512 {
        exp[i] = exp[i - 255];
        i += 1;
    }
    (exp, log)
}

const TABLES: ([u8; 512], [u8; 256]) = build_tables();
const EXP: &[u8; 512] = &TABLES.0;
const LOG: &[u8; 256] = &TABLES.1;

/// Flat 64 KiB multiply table: `MUL[(a << 8) | b] == a * b`.
static MUL: LazyLock<Box<[u8]>> = LazyLock::new(|| {
    let mut t = vec![0u8; 65536];
    for a in 0..256usize {
        for b in 0..256usize {
            t[(a << 8) | b] = mul(a as u8, b as u8);
        }
    }
    t.into_boxed_slice()
});

#[inline]
pub fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        EXP[LOG[a as usize] as usize + LOG[b as usize] as usize]
    }
}

#[inline]
pub fn inv(a: u8) -> u8 {
    debug_assert!(a != 0, "inverse of zero");
    EXP[255 - LOG[a as usize] as usize]
}

/// `dst[i] ^= coeff * src[i]` over the whole slice.
///
/// Scalar table lookup for now; the SIMD split-table variant lands with the
/// performance pass (PLAN.md §1.7) behind the same signature.
#[inline]
pub fn madd_into(dst: &mut [u8], src: &[u8], coeff: u8) {
    if coeff == 0 {
        return;
    }
    debug_assert_eq!(dst.len(), src.len());
    let base = (coeff as usize) << 8;
    let table = &MUL[base..base + 256];
    for (d, s) in dst.iter_mut().zip(src) {
        *d ^= table[*s as usize];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_roundtrip() {
        for a in 1..=255u8 {
            assert_eq!(mul(a, inv(a)), 1, "a={a}");
        }
    }

    #[test]
    fn known_products() {
        // Spot checks against the Python reference tables.
        assert_eq!(mul(2, 128), 0x1d);
        assert_eq!(mul(3, 7), 9);
        assert_eq!(inv(2), 0x8e);
    }
}
