// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! Seyd wire protocol — see `docs/adr/0001-wire-protocol-v2.md`.
//!
//! This crate is pure data layout: no I/O, no FEC, no policy. Both ends of a
//! link (Rust agent, TypeScript pilot) derive their parsers from the same ADR,
//! and the version nibble makes every format change a hard cutover: unknown
//! versions are counted and dropped, never guessed at.

pub mod v1;
pub mod v2;

/// Wrap-aware signed difference of two 16-bit sequence numbers (`a - b`).
///
/// Every frame-id comparison must go through this. The prototype had a real
/// bug from unsigned arithmetic: one reordered chunk from an older frame made
/// the eviction sweep compute `(old - current) & 0xFFFF ≈ 65500` for the frame
/// being assembled and delete it.
#[inline]
pub fn seq_delta(a: u16, b: u16) -> i16 {
    a.wrapping_sub(b) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_delta_wraps() {
        assert_eq!(seq_delta(5, 3), 2);
        assert_eq!(seq_delta(3, 5), -2);
        assert_eq!(seq_delta(2, 65535), 3);
        assert_eq!(seq_delta(65535, 2), -3);
    }
}
