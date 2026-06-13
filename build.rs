//! Build script: compile the Sail C++ model class + the C Sail runtime + shim.
//!
//! Default is fully self-contained — vendored pre-generated model + patched
//! runtime with bundled mini-gmp, no Sail/GMP/zlib. Env overrides for building
//! against a freshly generated model (see README Step 2):
//!   SAIL_MODEL_C   path to the generated model .cpp
//!   SAIL_MODEL_INC extra include dir (default: dir of SAIL_MODEL_C)
//!   SAIL_LIB_DIR   external (opam) Sail runtime dir; implies system GMP + zlib
//!   SAIL_SYSTEM_GMP=1  link real libgmp instead of bundled mini-gmp (also OK on
//!                  Windows: sail.h's LLP64 patch keeps 64-bit values intact).
//!                  Consumer-facing equivalent: the `system-gmp` cargo feature
//!                  (`system-gmp-static` for static linking). Auto-discovery
//!                  order: GMP_DIR, INCLUDE/LIB, structured PATH scan (bin/prefix
//!                  -> include|lib), gmp files directly on PATH, toolchain default.
//!   GMP_DIR        prefix of a custom libgmp; uses $GMP_DIR/{include,lib}
//!   GMP_INCLUDE_DIR / GMP_LIB_DIR  override either half of GMP_DIR
//!   GMP_LIB_NAME (default "gmp") / GMP_STATIC=1  link name / link statically
//!   ZLIB_LIB_DIR / ZLIB_LIB_NAME  zlib link tweaks (external runtime only)
//! The native model/runtime is force-built at >= -O2 (the exact-rational FP
//! paths are unusably slow at -O0); SAIL_MODEL_DEBUG=1 inherits the cargo
//! profile's own opt-level instead, for a fast unoptimized compile.

use std::env;
use std::path::{Path, PathBuf};

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

/// True if `dir` holds a linkable gmp (any platform's naming).
fn gmp_lib_present(dir: &Path) -> bool {
    for name in ["gmp.lib", "libgmp.a", "libgmp.so", "libgmp.dylib"] {
        if dir.join(name).is_file() {
            return true;
        }
    }
    // Versioned shared objects (libgmp.so.10, ...).
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().starts_with("libgmp.so"))
        })
        .unwrap_or(false)
}

/// First dir in a `;`/`:`-separated env path-list (e.g. MSVC's INCLUDE / LIB)
/// that satisfies `pred`.
fn find_dir_in_env(var: &str, pred: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    env::var_os(var).and_then(|v| env::split_paths(&v).find(|d| pred(d)))
}

/// Scan PATH for a system GMP and return (include_dir, lib_dir); either may be
/// None, in which case the toolchain's default search path is relied on for
/// that half. For each PATH entry it probes the dir itself, `<dir>/include`
/// (`/lib`), and the sibling `<dir>/../include` (`/../lib`) — so it works
/// whether a bin/ or the prefix is on PATH, or (common on Windows) the include/
/// and lib/ dirs themselves are, even as separate PATH entries.
fn scan_path_for_gmp() -> (Option<PathBuf>, Option<PathBuf>) {
    let path = match env::var_os("PATH") {
        Some(p) => p,
        None => return (None, None),
    };
    let mut inc = None;
    let mut lib = None;
    for dir in env::split_paths(&path) {
        let sibling = dir.parent().map(Path::to_path_buf);
        if inc.is_none() {
            let mut cands = vec![dir.join("include")];
            cands.extend(sibling.as_ref().map(|p| p.join("include")));
            inc = cands.into_iter().find(|c| c.join("gmp.h").is_file());
        }
        if lib.is_none() {
            let mut cands = vec![dir.join("lib")];
            cands.extend(sibling.as_ref().map(|p| p.join("lib")));
            lib = cands.into_iter().find(|c| gmp_lib_present(c));
        }
        if inc.is_some() && lib.is_some() {
            break;
        }
    }
    (inc, lib)
}

/// Last-resort PATH rule: gmp.h and a gmp lib sitting DIRECTLY in PATH dirs (the
/// two may be different entries). Catches installs that don't follow the
/// bin/include/lib prefix convention — anything that just dumped the files into
/// a dir that happens to be on PATH.
fn scan_path_flat() -> (Option<PathBuf>, Option<PathBuf>) {
    let path = match env::var_os("PATH") {
        Some(p) => p,
        None => return (None, None),
    };
    let mut inc = None;
    let mut lib = None;
    for dir in env::split_paths(&path) {
        if inc.is_none() && dir.join("gmp.h").is_file() {
            inc = Some(dir.clone());
        }
        if lib.is_none() && gmp_lib_present(&dir) {
            lib = Some(dir.clone());
        }
        if inc.is_some() && lib.is_some() {
            break;
        }
    }
    (inc, lib)
}

fn main() {
    for key in [
        "SAIL_MODEL_C", "SAIL_LIB_DIR", "SAIL_MODEL_INC", "SAIL_SYSTEM_GMP", "SAIL_MODEL_DEBUG",
        "GMP_DIR", "GMP_INCLUDE_DIR", "GMP_LIB_DIR", "GMP_LIB_NAME", "GMP_STATIC",
        "ZLIB_LIB_DIR", "ZLIB_LIB_NAME",
    ] {
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
        Err(_) => (unpack_vendored_model(&vendor), vendor.clone()),
    };

    // Runtime selection: default = vendored patched runtime (self-contained,
    // bundled mini-gmp, no elf.c/zlib); SAIL_LIB_DIR = external runtime (system
    // GMP + zlib); SAIL_SYSTEM_GMP or the `system-gmp` feature = vendored sources
    // but real libgmp (`system-gmp-static`/GMP_STATIC for static linking).
    let feature_system_gmp = env::var_os("CARGO_FEATURE_SYSTEM_GMP").is_some();
    let system_runtime = env::var_os("SAIL_LIB_DIR").is_some();
    let system_gmp =
        system_runtime || feature_system_gmp || env::var_os("SAIL_SYSTEM_GMP").is_some();
    let gmp_static = env::var_os("CARGO_FEATURE_SYSTEM_GMP_STATIC").is_some()
        || env::var_os("GMP_STATIC").is_some();
    let sail_lib = env::var("SAIL_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| vendor.join("sail-runtime"));
    assert!(sail_lib.is_dir(), "Sail runtime dir does not exist: {sail_lib:?}");

    // Resolve the system GMP's include/lib dirs (explicit env wins, else discover
    // per the module-doc order). Feeding the header path through cc's .include()
    // is portable across MSVC and gcc, so consumers don't hand-set CPATH/CFLAGS.
    let gmp_dir = env::var_os("GMP_DIR").map(PathBuf::from);
    let explicit_include = env::var_os("GMP_INCLUDE_DIR")
        .map(PathBuf::from)
        .or_else(|| gmp_dir.as_ref().map(|p| p.join("include")));
    let explicit_lib = env::var_os("GMP_LIB_DIR")
        .map(PathBuf::from)
        .or_else(|| gmp_dir.as_ref().map(|p| p.join("lib")));
    let (gmp_include, gmp_lib_dir) = if explicit_include.is_some() || explicit_lib.is_some() {
        (explicit_include, explicit_lib)
    } else if system_gmp {
        let (s_inc, s_lib) = scan_path_for_gmp();
        let (f_inc, f_lib) = scan_path_flat();
        let inc = find_dir_in_env("INCLUDE", |d| d.join("gmp.h").is_file())
            .or(s_inc)
            .or(f_inc);
        let lib = find_dir_in_env("LIB", gmp_lib_present).or(s_lib).or(f_lib);
        (inc, lib)
    } else {
        (None, None)
    };

    // Hard guarantee: an explicit system-gmp request never silently falls back to
    // the bundled mini-gmp. Probe that <gmp.h> compiles and fail loudly now,
    // rather than grinding through the whole model build into a cryptic error.
    if system_gmp {
        let out = PathBuf::from(env::var("OUT_DIR").unwrap());
        let probe_src = out.join("gmp_probe.c");
        std::fs::write(
            &probe_src,
            "#include <gmp.h>\nint sail_gmp_probe(void){mpz_t z;mpz_init(z);mpz_clear(z);return 0;}\n",
        )
        .expect("write gmp probe source");
        let mut probe = cc::Build::new();
        probe.file(&probe_src).cargo_metadata(false).warnings(false);
        if let Some(inc) = &gmp_include {
            probe.include(inc);
        }
        if probe.try_compile("sail_gmp_probe").is_err() {
            panic!(
                "system-gmp is requested (cargo feature `system-gmp`/`system-gmp-static` or \
                 SAIL_SYSTEM_GMP) but <gmp.h> could not be compiled. Point the build at a libgmp \
                 via GMP_DIR=<prefix> (with include/ and lib/), GMP_INCLUDE_DIR/GMP_LIB_DIR, \
                 INCLUDE/LIB, PATH, or a system-wide install. This build does NOT fall back to \
                 the bundled mini-gmp."
            );
        }
    }

    // Floor the native model/runtime at -O2 even in dev builds: at the cargo
    // dev profile's -O0 the exact-rational FP paths (hundreds of mpq ops per
    // instruction) run ~20-50x slower, and this generated code is never
    // single-stepped, so there is no reason to leave it unoptimized.
    // SAIL_MODEL_DEBUG=1 opts out (inherit the profile's level verbatim) for a
    // fast unoptimized compile when runtime speed doesn't matter.
    let model_opt = if env::var_os("SAIL_MODEL_DEBUG").is_some() {
        env::var("OPT_LEVEL").unwrap_or_else(|_| "0".to_string())
    } else {
        match env::var("OPT_LEVEL").as_deref() {
            Ok("2") | Ok("3") | Ok("s") | Ok("z") => env::var("OPT_LEVEL").unwrap(),
            _ => "2".to_string(),
        }
    };

    let common_flags = |b: &mut cc::Build| {
        b.opt_level_str(&model_opt);
        // Sail-generated code is not warning-clean; keep logs readable.
        b.flag_if_supported("-Wno-unused")
            .flag_if_supported("-Wno-unused-parameter")
            .flag_if_supported("-Wno-unused-but-set-variable")
            .flag_if_supported("-Wno-sign-compare")
            .flag_if_supported("-Wno-extra");
    };

    // ---- 1) C++: generated model class + shim ------------------------------
    // Must be compiled FIRST: the model archive references runtime symbols, so
    // it must precede libsailruntime on traditional linkers.
    // C++20 required: generated code uses designated initializers + the `not`
    // token; MSVC needs /std:c++20 (implies /permissive-) for those.
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
    // Methods the --cpp backend declares but never emits (generated by
    // scripts/fix_cpp_model.py; ships in vendor/ for the pre-generated model).
    let missing = model_inc.join("model_missing.cpp");
    if missing.is_file() {
        cxx.file(missing);
    }
    common_flags(&mut cxx);
    if system_gmp {
        cxx.define("SAIL_SYSTEM_GMP", None); // sail.h: include <gmp.h>
        if let Some(inc) = &gmp_include {
            cxx.include(inc);
        }
    }
    cxx.compile("sailarm");

    // ---- 2) C: Sail runtime -------------------------------------------------
    let mut crt = cc::Build::new();
    crt.include(&sail_lib)
        .flag_if_supported("/std:c11") // timespec_get etc. under MSVC
        .file(sail_lib.join("sail.c"))
        .file(sail_lib.join("rts.c"));
    common_flags(&mut crt);
    // Sail >= 0.18 split the runtime: sail_assert (sail_failure.c) and
    // sail_config.c (needs cJSON.c) are referenced by the generated code.
    for extra in ["sail_failure.c", "sail_config.c", "cJSON.c"] {
        let p = sail_lib.join(extra);
        if p.is_file() {
            crt.file(p);
        }
    }
    if system_gmp {
        crt.define("SAIL_SYSTEM_GMP", None);
        if let Some(inc) = &gmp_include {
            crt.include(inc);
        }
    } else {
        crt.file(vendor.join("sail-runtime/mini-gmp.c"))
            .file(vendor.join("sail-runtime/mini-mpq.c"));
    }
    if system_runtime {
        // Only external runtimes compile elf.c; the model references nothing in it.
        crt.file(sail_lib.join("elf.c"));
    }
    crt.compile("sailruntime");

    // ---- native link deps ----------------------------------------------------
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    if let Some(dir) = &gmp_lib_dir {
        println!("cargo:rustc-link-search=native={}", dir.display());
    }
    if let Ok(dir) = env::var("ZLIB_LIB_DIR") {
        println!("cargo:rustc-link-search=native={dir}");
    }
    if system_gmp {
        let gmp = env::var("GMP_LIB_NAME").unwrap_or_else(|_| "gmp".into());
        let kind = if gmp_static { "static=" } else { "" };
        println!("cargo:rustc-link-lib={kind}{gmp}");
    }
    if system_runtime {
        let zlib = env::var("ZLIB_LIB_NAME")
            .unwrap_or_else(|_| if target_env == "msvc" { "zlib".into() } else { "z".into() });
        println!("cargo:rustc-link-lib={zlib}"); // external elf.c may use zlib
    }

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
