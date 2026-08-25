//! Mirrors the target test `sp1-core-executor` gates its native JIT on, since the SP1 backend can
//! only read guest memory where that JIT is the executor.

use std::env;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(sp1_jit_memory)");

    // `sp1-core-executor` compiles its native JIT for this target, and only that JIT keeps guest
    // memory in a file the SP1 backend can read the heap out of. Its own test also excludes two of
    // its features, which a dependent cannot see, so enabling either fails this crate's build
    // rather than quietly changing what it measures.
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64")
        && env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
    {
        println!("cargo::rustc-cfg=sp1_jit_memory");
    }
}
