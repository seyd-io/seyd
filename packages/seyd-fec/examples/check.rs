//! Replay FEC interop vectors from `tools/fec-vectors.py` through the Rust coder.
//!
//!     python3 tools/fec-vectors.py | cargo run -p seyd-fec --example check
//!
//! Proves the Rust implementation agrees with the Python reference byte for
//! byte: field tables, Cauchy matrix, legacy v1 header layout, and the
//! `last_len` trimming a reconstructed final chunk depends on. The TypeScript
//! pilot runs the same vectors through `tools/fec-check.js`.

use serde::Deserialize;
use std::io::Read;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Vectors {
    seed: u64,
    chunk_size: usize,
    header_len: usize,
    version: u8,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    label: String,
    n: usize,
    k: usize,
    last_len: usize,
    is_keyframe: bool,
    frame_id: u16,
    chunks: Vec<String>,
    erased: Vec<usize>,
    expected: String,
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}

fn main() {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw).unwrap();
    if raw.trim().is_empty() {
        eprintln!("no input — pipe tools/fec-vectors.py into this program");
        std::process::exit(2);
    }
    let vec: Vectors = serde_json::from_str(&raw).expect("invalid vector JSON");
    if vec.header_len != seyd_wire::v1::HEADER_LEN || vec.version != seyd_wire::v1::VERSION {
        eprintln!(
            "FAIL wire mismatch: vectors headerLen={} v{}, seyd-wire v1 headerLen={} v{}",
            vec.header_len,
            vec.version,
            seyd_wire::v1::HEADER_LEN,
            seyd_wire::v1::VERSION
        );
        std::process::exit(1);
    }

    let (mut pass, mut fail) = (0, 0);
    for c in &vec.cases {
        let mut data: Vec<Option<Vec<u8>>> = vec![None; c.n];
        let mut parity: Vec<Option<Vec<u8>>> = vec![None; c.k];
        let mut header_ok = true;
        for (i, h) in c.chunks.iter().enumerate() {
            if c.erased.contains(&i) {
                continue;
            }
            let bytes = hex(h);
            let Some((hdr, payload)) = seyd_wire::v1::parse(&bytes) else {
                header_ok = false;
                break;
            };
            if hdr.n as usize != c.n
                || hdr.k as usize != c.k
                || hdr.last_len as usize != c.last_len
                || hdr.frame_id != c.frame_id
                || hdr.keyframe != c.is_keyframe
            {
                header_ok = false;
                break;
            }
            if (hdr.chunk_idx as usize) < c.n {
                let mut body = payload.to_vec();
                body.resize(vec.chunk_size, 0);
                data[hdr.chunk_idx as usize] = Some(body);
            } else {
                parity[hdr.chunk_idx as usize - c.n] = Some(payload.to_vec());
            }
        }
        if !header_ok {
            eprintln!("FAIL header  {}", c.label);
            fail += 1;
            continue;
        }
        if !seyd_fec::decode(&mut data, &parity) {
            eprintln!("FAIL unrecoverable  {}", c.label);
            fail += 1;
            continue;
        }
        let mut rebuilt = Vec::with_capacity((c.n - 1) * vec.chunk_size + c.last_len);
        for (i, chunk) in data.iter().enumerate() {
            let take = if i == c.n - 1 {
                c.last_len
            } else {
                vec.chunk_size
            };
            rebuilt.extend_from_slice(&chunk.as_ref().unwrap()[..take]);
        }
        if rebuilt == hex(&c.expected) {
            pass += 1;
        } else {
            eprintln!("FAIL mismatch  {}", c.label);
            fail += 1;
        }
    }
    println!(
        "fec interop (rust): {pass} passed, {fail} failed (seed {})",
        vec.seed
    );
    std::process::exit(if fail > 0 { 1 } else { 0 });
}
