//! Build script: compile the Sail-generated C++ model class + the (C) Sail
//! runtime + our shim.
//!
//! Out of the box this uses the PRE-GENERATED model vendored in `vendor/`
//! (`model.cpp.gz` + `model.h` + the portability-patched Sail C runtime in
//! `vendor/sail-runtime/` with bundled mini-gmp), so the build is fully
//! self-contained: no Sail/OCaml/z3, no GMP, no zlib — just C and C++
//! compilers.
//!
//! The model is a C++ class (`sail --cpp`): all architectural state is per
//! instance, which is what allows multiple independent `Oracle`s per process.
//!
//! To build against a freshly generated model instead (see README Step 2),
//! override with environment variables:
//!   SAIL_MODEL_C   absolute path to the generated model .cpp
//!   SAIL_LIB_DIR   dir containing an external Sail runtime (e.g. opam's
//!                  `$(opam var share)/sail/lib`); implies system GMP + zlib
//! Optional:
//!   SAIL_MODEL_INC extra include dir (defaults to the dir of SAIL_MODEL_C)
//!   SAIL_SYSTEM_GMP=1  link real libgmp instead of the bundled mini-gmp
//!   GMP_LIB_DIR / ZLIB_LIB_DIR / GMP_LIB_NAME / ZLIB_LIB_NAME  link tweaks

use std::env;
use std::path::{Path, PathBuf};

/// Decompress `vendor/model.cpp.gz` into OUT_DIR/model.cpp (only when stale)
/// and return its path.
fn unpack_vendored_model(vendor: &Path) -> PathBuf {
    let gz = vendor.join("model.cpp.gz");
    assert!(
        gz.is_file(),
        "vendor/model.cpp.gz is missing and SAIL_MODEL_C is not set. \
         Either restore the vendored model or generate one (README Step 2)."
    );
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("model.cpp");

    let stale = match (out.metadata(), gz.metadata()) {
        (Ok(o), Ok(g)) => match (o.modified(), g.modified()) {
            (Ok(o), Ok(g)) => o < g,
            _ => true,
        },
        _ => true,
    };
    if stale {
        let mut reader =
            flate2::read::GzDecoder::new(std::fs::File::open(&gz).expect("open model.cpp.gz"));
        let mut writer = std::io::BufWriter::new(
            std::fs::File::create(&out).expect("create OUT_DIR/model.cpp"),
        );
        std::io::copy(&mut reader, &mut writer).expect("decompress model.cpp.gz");
    }
    out
}

fn main() {
    for key in ["SAIL_MODEL_C", "SAIL_LIB_DIR", "SAIL_MODEL_INC", "SAIL_SYSTEM_GMP"] {
        println!("cargo:rerun-if-env-changed={key}");
    }

    let vendor = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("vendor");

    let (model_src, model_inc) = match env::var("SAIL_MODEL_C") {
        Ok(p) => {
            let src = PathBuf::from(p);
            assert!(src.is_file(), "SAIL_MODEL_C does not exist: {src:?}");
            let inc = env::var("SAIL_MODEL_INC")
                .map(PathBuf::from)
                .unwrap_or_else(|_| src.parent().unwrap().to_path_buf());
            (src, inc)
        }
        // Default: vendored pre-generated model (model.h lives in vendor/).
        Err(_) => (unpack_vendored_model(&vendor), vendor.clone()),
    };

    // Runtime selection:
    //  - default: the PATCHED runtime in vendor/sail-runtime — fully
    //    self-contained (bundled mini-gmp/mini-mpq, no elf.c, no zlib, no
    //    system libraries at all);
    //  - SAIL_LIB_DIR set: an external (e.g. opam) runtime — links system
    //    GMP + zlib as upstream Sail expects;
    //  - SAIL_SYSTEM_GMP set (with the vendored runtime): keep the vendored
    //    sources but use real libgmp for faster arbitrary-precision paths.
    let system_runtime = env::var_os("SAIL_LIB_DIR").is_some();
    let system_gmp = system_runtime || env::var_os("SAIL_SYSTEM_GMP").is_some();
    let sail_lib = env::var("SAIL_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| vendor.join("sail-runtime"));
    assert!(sail_lib.is_dir(), "Sail runtime dir does not exist: {sail_lib:?}");

    let warning_flags = |b: &mut cc::Build| {
        // Sail-generated code is large and not warning-clean; keep logs readable.
        b.flag_if_supported("-Wno-unused")
            .flag_if_supported("-Wno-unused-parameter")
            .flag_if_supported("-Wno-unused-but-set-variable")
            .flag_if_supported("-Wno-sign-compare")
            .flag_if_supported("-Wno-extra");
    };

    // ---- 1) C++: generated model class + shim ------------------------------
    // Compiled (and link-emitted) FIRST: the model archive references runtime
    // symbols, so it must precede libsailruntime on traditional linkers.
    // C++20: the generated code uses designated initializers and the `not`
    // alternative token — fine for g++/clang in any mode, but MSVC needs
    // /std:c++20 (which implies /permissive-) for both.
    let mut cxx = cc::Build::new();
    cxx.cpp(true)
        .std("c++20")
        .include(&sail_lib)
        .include(&model_inc)
        .include("csrc")
        // MSVC: model object exceeds the default COFF section limit.
        .flag_if_supported("/bigobj")
        .file(&model_src)
        .file("csrc/shim.cpp");
    // Definitions for methods the --cpp backend declares but never emits;
    // generated by scripts/fix_cpp_model.py, ships in vendor/ for the
    // pre-generated model.
    let missing = model_inc.join("model_missing.cpp");
    if missing.is_file() {
        cxx.file(missing);
    }
    warning_flags(&mut cxx);
    if system_gmp {
        cxx.define("SAIL_SYSTEM_GMP", None); // sail.h: include <gmp.h>
    }
    cxx.compile("sailarm");

    // ---- 2) C: Sail runtime -------------------------------------------------
    let mut crt = cc::Build::new();
    crt.include(&sail_lib)
        .flag_if_supported("/std:c11") // timespec_get etc. under MSVC
        .file(sail_lib.join("sail.c"))
        .file(sail_lib.join("rts.c"));
    warning_flags(&mut crt);
    // Sail >= 0.18 split the runtime further; the generated code references
    // sail_assert (sail_failure.c) and model.h includes sail_config.h
    // (sail_config.c, which itself needs the bundled cJSON.c).
    for extra in ["sail_failure.c", "sail_config.c", "cJSON.c"] {
        let p = sail_lib.join(extra);
        if p.is_file() {
            crt.file(p);
        }
    }
    if system_gmp {
        crt.define("SAIL_SYSTEM_GMP", None);
    } else {
        // Bundled pure-C arbitrary precision; no system libraries needed.
        crt.file(vendor.join("sail-runtime/mini-gmp.c"))
            .file(vendor.join("sail-runtime/mini-mpq.c"));
    }
    if system_runtime {
        // External runtimes ship elf.c and may need zlib; the vendored runtime
        // doesn't compile elf.c at all (nothing in the model references it).
        crt.file(sail_lib.join("elf.c"));
    }
    crt.compile("sailruntime");

    // ---- native link deps ----------------------------------------------------
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    if let Ok(dir) = env::var("GMP_LIB_DIR") {
        println!("cargo:rustc-link-search=native={dir}");
    }
    if let Ok(dir) = env::var("ZLIB_LIB_DIR") {
        println!("cargo:rustc-link-search=native={dir}");
    }
    // System libraries are only needed outside the self-contained default.
    if system_gmp {
        let gmp = env::var("GMP_LIB_NAME").unwrap_or_else(|_| "gmp".into());
        println!("cargo:rustc-link-lib={gmp}");
    }
    if system_runtime {
        let zlib = env::var("ZLIB_LIB_NAME")
            .unwrap_or_else(|_| if target_env == "msvc" { "zlib".into() } else { "z".into() });
        println!("cargo:rustc-link-lib={zlib}"); // external elf.c may use zlib
    }

    // Rebuild triggers.
    for p in [
        model_src.as_path(),
        Path::new("vendor/model.cpp.gz"),
        Path::new("vendor/sail-runtime"),
        Path::new("csrc/shim.cpp"),
        Path::new("build.rs"),
    ] {
        println!("cargo:rerun-if-changed={}", p.display());
    }
}
