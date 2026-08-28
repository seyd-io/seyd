//! Reed-Solomon erasure coding over GF(256) with a Cauchy generator matrix.
//!
//! Rationale, measurements and history live in PLAN.md §1.7 and in the legacy
//! Python reference `packages/agent/fec.py`. What matters for this crate:
//!
//! * `k` parity chunks over `n` equal-length data chunks recover **any** `k`
//!   erasures. Cauchy rather than Vandermonde because every square submatrix
//!   of a Cauchy matrix is invertible, which is exactly that guarantee.
//! * The construction — poly `0x11d`, `A[i][j] = 1 / (i XOR (k + j))` — is
//!   part of the wire contract. Changing it breaks interop silently, which is
//!   why `tools/fec-vectors.py | cargo run -p seyd-fec --example check` exists.
//! * The coder is payload-agnostic: it never parses what it protects.

pub mod gf;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Hard limit of the construction: `n + k <= 256`.
pub const MAX_SYMBOLS: usize = 256;

/// `k x n` Cauchy matrix, cached per `(n, k)`.
///
/// `x_i = i` for `i in 0..k`, `y_j = k + j` for `j in 0..n`; the sets are
/// disjoint so nothing is inverted at zero, and distinct within each set,
/// which is what makes every square submatrix invertible.
pub type Matrix = std::sync::Arc<Vec<Vec<u8>>>;

pub fn cauchy_matrix(n: usize, k: usize) -> Matrix {
    static CACHE: OnceLock<Mutex<HashMap<(usize, usize), Matrix>>> = OnceLock::new();
    assert!(n + k <= MAX_SYMBOLS, "n+k must be <= 256, got n={n} k={k}");
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap();
    guard
        .entry((n, k))
        .or_insert_with(|| {
            let m = (0..k)
                .map(|i| (0..n).map(|j| gf::inv((i ^ (k + j)) as u8)).collect())
                .collect();
            std::sync::Arc::new(m)
        })
        .clone()
}

/// Parity chunks for an `n`-chunk block at `pct` overhead.
///
/// Always at least 1 when `pct > 0`: a tiny block still deserves protection,
/// and `n = 1, k = 1` degenerates to a plain duplicate, which is right.
pub fn parity_count(n: usize, pct: u32, cap: usize) -> usize {
    if pct == 0 || n == 0 {
        return 0;
    }
    // Python's round() is banker's rounding; ties here are only possible at
    // .5 exactly, which the vector suite covers. Match it precisely.
    let scaled = (n as f64) * (pct as f64) / 100.0;
    let rounded = round_half_even(scaled);
    rounded.max(1).min(cap).min(n)
}

fn round_half_even(x: f64) -> usize {
    let floor = x.floor();
    let diff = x - floor;
    let round_up = diff > 0.5 || (diff == 0.5 && (floor as u64) % 2 == 1);
    (if round_up { floor + 1.0 } else { floor }) as usize
}

/// Compute `k` parity chunks from `n` equal-length data chunks.
///
/// Every chunk must be exactly the same length — the caller zero-pads the
/// short final one and carries its true length in the wire header.
pub fn encode_parity(chunks: &[&[u8]], k: usize) -> Vec<Vec<u8>> {
    if k == 0 || chunks.is_empty() {
        return Vec::new();
    }
    let size = chunks[0].len();
    debug_assert!(
        chunks.iter().all(|c| c.len() == size),
        "unequal chunk sizes"
    );
    let matrix = cauchy_matrix(chunks.len(), k);
    matrix
        .iter()
        .map(|row| {
            let mut acc = vec![0u8; size];
            for (&coeff, chunk) in row.iter().zip(chunks) {
                gf::madd_into(&mut acc, chunk, coeff);
            }
            acc
        })
        .collect()
}

/// Reconstruct missing data chunks in place. Returns `false` if unrecoverable.
///
/// Only the erased data positions are solved for, so the system is `d x d`
/// where `d` is the number of erasures rather than `n x n`.
pub fn decode(data: &mut [Option<Vec<u8>>], parity: &[Option<Vec<u8>>]) -> bool {
    let n = data.len();
    let k = parity.len();
    let lost: Vec<usize> = (0..n).filter(|&i| data[i].is_none()).collect();
    if lost.is_empty() {
        return true;
    }
    let available: Vec<usize> = (0..k).filter(|&p| parity[p].is_some()).collect();
    if available.len() < lost.len() {
        return false;
    }
    let use_rows = &available[..lost.len()];
    let size = match data
        .iter()
        .chain(parity.iter())
        .find_map(|c| c.as_ref().map(|v| v.len()))
    {
        Some(s) => s,
        None => return false,
    };
    let matrix = cauchy_matrix(n, k);

    // Syndromes: each chosen parity chunk minus the contribution of the data
    // we still have; what remains is that row's combination of the lost chunks.
    let syndromes: Vec<Vec<u8>> = use_rows
        .iter()
        .map(|&p| {
            let mut acc = parity[p].as_ref().unwrap().clone();
            for j in 0..n {
                if let Some(chunk) = &data[j] {
                    gf::madd_into(&mut acc, chunk, matrix[p][j]);
                }
            }
            acc
        })
        .collect();

    let sub: Vec<Vec<u8>> = use_rows
        .iter()
        .map(|&p| lost.iter().map(|&j| matrix[p][j]).collect())
        .collect();
    let inverse = match invert(&sub) {
        Some(m) => m,
        None => return false,
    };

    for (r, &idx) in lost.iter().enumerate() {
        let mut acc = vec![0u8; size];
        for (c, syndrome) in syndromes.iter().enumerate() {
            gf::madd_into(&mut acc, syndrome, inverse[r][c]);
        }
        data[idx] = Some(acc);
    }
    true
}

/// Gauss-Jordan inverse of a square GF(256) matrix.
fn invert(matrix: &[Vec<u8>]) -> Option<Vec<Vec<u8>>> {
    let m = matrix.len();
    let mut aug: Vec<Vec<u8>> = matrix
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut r = row.clone();
            r.extend((0..m).map(|j| u8::from(i == j)));
            r
        })
        .collect();
    for col in 0..m {
        let pivot = (col..m).find(|&r| aug[r][col] != 0)?;
        aug.swap(col, pivot);
        let scale = gf::inv(aug[col][col]);
        for v in aug[col].iter_mut() {
            *v = gf::mul(*v, scale);
        }
        let pivot_row = aug[col].clone();
        for (r, row) in aug.iter_mut().enumerate() {
            if r != col && row[col] != 0 {
                let f = row[col];
                for (v, p) in row.iter_mut().zip(&pivot_row) {
                    *v ^= gf::mul(f, *p);
                }
            }
        }
    }
    Some(aug.into_iter().map(|row| row[m..].to_vec()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    fn random_chunks(rng: &mut Rng, n: usize, size: usize) -> Vec<Vec<u8>> {
        (0..n)
            .map(|_| (0..size).map(|_| rng.next() as u8).collect())
            .collect()
    }

    #[test]
    fn any_k_erasures_recover() {
        let mut rng = Rng(0x9e3779b97f4a7c15);
        for &(n, k) in &[(1, 1), (6, 2), (12, 4), (18, 9), (50, 16), (200, 56)] {
            let chunks = random_chunks(&mut rng, n, 64);
            let refs: Vec<&[u8]> = chunks.iter().map(|c| c.as_slice()).collect();
            let parity = encode_parity(&refs, k);
            for _trial in 0..20 {
                let mut data: Vec<Option<Vec<u8>>> = chunks.iter().cloned().map(Some).collect();
                let mut par: Vec<Option<Vec<u8>>> = parity.iter().cloned().map(Some).collect();
                let mut erased = 0;
                while erased < k {
                    let i = rng.below(n + k);
                    let slot = if i < n { &mut data[i] } else { &mut par[i - n] };
                    if slot.is_some() {
                        *slot = None;
                        erased += 1;
                    }
                }
                assert!(decode(&mut data, &par), "n={n} k={k}");
                for (a, b) in data.iter().zip(&chunks) {
                    assert_eq!(a.as_ref().unwrap(), b);
                }
            }
        }
    }

    #[test]
    fn too_many_erasures_fail() {
        let chunks = [vec![1u8; 8], vec![2u8; 8], vec![3u8; 8]];
        let refs: Vec<&[u8]> = chunks.iter().map(|c| c.as_slice()).collect();
        let parity = encode_parity(&refs, 1);
        let mut data = vec![None, None, Some(chunks[2].clone())];
        let par: Vec<Option<Vec<u8>>> = parity.into_iter().map(Some).collect();
        assert!(!decode(&mut data, &par));
    }

    #[test]
    fn parity_count_matches_python() {
        assert_eq!(parity_count(6, 25, 16), 2);
        assert_eq!(parity_count(6, 8, 16), 1);
        assert_eq!(parity_count(18, 50, 16), 9);
        assert_eq!(parity_count(61, 30, 16), 16);
        assert_eq!(parity_count(1, 50, 16), 1);
        assert_eq!(parity_count(5, 0, 16), 0);
        // banker's rounding: 2.5 -> 2, 3.5 -> 4
        assert_eq!(parity_count(10, 25, 16), 2);
        assert_eq!(parity_count(14, 25, 16), 4);
    }

    #[test]
    fn cauchy_matches_reference() {
        let m = cauchy_matrix(3, 2);
        assert_eq!(m[0][0], gf::inv(2));
        assert_eq!(m[1][2], gf::inv(1 ^ 4));
    }
}
