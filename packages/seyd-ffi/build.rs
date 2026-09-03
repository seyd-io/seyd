//! Generate `sdks/c/include/seyd.h` from this crate (ADR 0004: the header is
//! cbindgen output, never hand-written). The file is checked in so that C and
//! Python consumers do not need a Rust toolchain; it is rewritten only when
//! the contents actually change, to keep `make`-style timestamps honest.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    let crate_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let header = crate_dir.join("../../sdks/c/include/seyd.h");

    let generated = match cbindgen::generate(&crate_dir) {
        Ok(b) => {
            let mut out = Vec::new();
            b.write(&mut out);
            out
        }
        Err(e) => {
            // A missing cbindgen must not break a build of the library itself;
            // the checked-in header is still valid.
            println!("cargo:warning=cbindgen failed, keeping existing seyd.h: {e}");
            return;
        }
    };
    if std::fs::read(&header).ok().as_deref() != Some(generated.as_slice()) {
        if let Some(dir) = header.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(&header, &generated).expect("write sdks/c/include/seyd.h");
    }
}
