# sail-aarch64-oracle

A Rust crate that embeds the authoritative **Sail `arm-v9.4-a`** ISA model
(rems-project's translation of Arm's official 2023-03 ASL specification) as
an **in-process golden-reference CPU** for differential testing: set up
architectural state, execute one A64 instruction at a time, and diff every
register, flag and memory effect against your own emulator/JIT/decoder.

## Features

- **Single-step oracle**: `step(opcode)` decodes + executes any 32-bit A64
  instruction through the model's own `__DecodeA64`, including the
  architectural exception path for UNDEF/trapping encodings.
- **Multi-instance**: every `Oracle::new()` is a fully independent CPU —
  registers AND memory are per instance; create, interleave and drop freely.
- **Thread-safe**: `Oracle` is `Send + Sync` (a process-wide mutex serializes
  model calls — safety, not parallel speedup; use `cargo nextest` processes
  for parallelism).
- **Self-contained & cross-platform**: the pre-generated model and a
  portability-patched Sail runtime are vendored; building needs only Rust +
  C/C++ compilers (C++20). Verified on Linux (musl) and Windows 11 (MSVC and
  MinGW). No Sail/OCaml/z3/GMP/zlib installation, ever.
- **Deterministic**: no interrupt delivery, no ticking counters, frozen
  cycle clock — the same inputs give the same outputs (exception: `RNDR`).

## Design

```
sail-arm/arm-v9.4-a  +  sail/harness.sail          (accessor + step wrappers)
        │  sail --cpp --c-no-main  (+ scripts/fix_cpp_model.py)
        ▼
  model.cpp / model.h   (class model::Model — ALL architectural state
        │                is instance members; ~166 MB, vendored as 18 MB .gz)
        │  cc (build.rs): model.cpp + model_missing.cpp + csrc/shim.cpp [C++20]
        │                 + patched Sail C runtime (bundled mini-gmp)   [C]
        ▼
   libsailarm.a  ──extern "C" handle ABI──►  src/lib.rs  (Oracle: Send+Sync)
```

Key decisions:

- **Sail's C++ backend** (`--cpp`) instead of the classic C backend: state
  becomes class members, enabling multiple independent instances per process
  (the C backend keeps everything in process globals — one CPU per process).
- **Per-instance memory**: the Sail runtime's memory model is process-global;
  the shim swaps each instance's block lists into the globals around every
  call (single-threaded under the lock, so this is exact).
- **Chunked 64-bit register ABI**: wide registers (Z/P/FFR/ZA/ZT0, up to
  2048 bits) cross the FFI as plain `u64` chunks over the architectural
  storage — no GMP types in the interface.
- **Reset state**: highest EL (EL3), MMU off (flat physical memory), SVE +
  FP + SME enabled untrapped, VL = SVL = 2048 bits. One `step` = decode +
  execute + PC advance (+4 or branch target); no instruction fetch from
  memory — the opcode is injected directly.

## Supported instructions

Everything the arm-v9.4-a model decodes via A64, i.e. the full Armv9.4-A
instruction set as specified by Arm's ASL (feature gates follow the model's
defaults — essentially all v9.4 features implemented, including SVE2,
SME/SME2, LSE atomics, MTE):

| Category | Notes | Covered by tests |
|---|---|---|
| Base integer | arith/logic/bitfield/branch/CSEL… | ADD/SUB |
| Loads/stores | incl. SP-relative, exclusives, atomics, pairs | STR/LDR round-trips; direct RAM API matches |
| FP scalar & AdvSIMD | FPCR rounding, FPSR flag accumulation | FADD, FDIV→DZC |
| SVE/SVE2 | VL = 2048 b (runtime-settable), predicates, FFR | ADD Z.D, PTRUE, RDFFR, set_vl |
| SME/SME2 | streaming mode, ZA, ZT0; SVL = 2048 b | SMSTART, ZERO {ZA} |
| System | MRS/MSR, hints, barriers, exception-generating | MRS/MSR TPIDR_EL0, UDF |

State access API (everything a diff harness can set/observe):

| State | API |
|---|---|
| X0–X30, SP, PC, NZCV | `set_x/get_x`, `set_sp/get_sp`, `set_pc/get_pc`, `set_nzcv/get_nzcv` |
| PSTATE (DAIF/BTYPE/SSBS/PAN/UAO/DIT/TCO/SPSel; EL/SM/ZA read-only) | `set_pstate/get_pstate` (packed word) |
| Z0–Z31 (2048 b), P0–P15 (256 b), FFR | `set_z/get_z`, `set_p/get_p`, `set_ffr/get_ffr` |
| Vector length (VL / streaming SVL) | `vl_bits()` (read); `set_vl(bits)`, `set_svl(bits)` (program ZCR/SMCR.LEN at runtime) |
| FPCR, FPSR | `set_fpcr/get_fpcr`, `set_fpsr/get_fpsr` |
| ZA (256×2048 b), ZT0 (512 b), SVCR | `set_za_row/get_za_row`, `set_zt0/get_zt0`, `svcr()` |
| TPIDR_EL0, TPIDRRO_EL0 | get/set |
| Memory (per-instance, byte-addressed, little-endian) | `read_mem_byte/write_mem_byte`, `read_mem/write_mem` (slices), `read_mem_u64/write_mem_u64`, `is_mapped(addr)` |
| Exception diagnosis | `esr_el3()`, `elr_el3()`, `far_el3()` (read-only) |

## Boundaries & limitations

- **A64 only**: AArch32 (A32/T32) is present in the model but not exposed
  (`step` enters `__DecodeA64`; an A32 entry point would follow the same
  harness recipe if ever needed).
- **No instruction fetch**: opcodes are injected, not read from model memory
  — self-modifying-code and ifetch-side effects are out of scope.
- **EL3, MMU off**: flat physical addressing. The full translation machinery
  exists in the model and is reachable by MSR-configuring
  SCTLR/TTBR/TCR + ERET to lower ELs through `step`, but the oracle does not
  set that up for you.
- **No asynchronous events**: interrupts are never delivered, timers/counters
  do not tick (CNT* reads are constant), WFI/WFE never wake. This is a
  feature (determinism), not a bug.
- **Nondeterministic instructions**: `RNDR`/`RNDRRS` use the runtime RNG —
  exclude them from diff campaigns or pin the seed.
- **Threading**: cross-thread use is safe but serialized by one global lock;
  parallel throughput comes from processes (`cargo nextest`), not threads.
- **Instance creation cost**: ~0.4 s (full model reset); individual steps are
  sub-millisecond. Prefer one instance per test/case, not per instruction.
- **Licensing**: crate code is BSD-3-Clause; the vendored model is
  BSD-3-Clause-Clear (Arm), the Sail runtime BSD-2-Clause, but bundled
  **mini-gmp is LGPLv3+/GPLv2+** — see `vendor/README.md` before
  redistributing binaries.

> ✅ Verified (2026-06, Sail 0.20.1, sail-arm master) on Linux/musl and
> Windows 11 MSVC — 13 tests, including multi-instance independence, memory
> isolation, direct-RAM/instruction agreement and cross-thread stress. Non-obvious toolchain findings (Sail
> DCE needs `--c-preserve`, `z`→`zz` name mangling, four cpp-backend bugs
> auto-fixed by `scripts/fix_cpp_model.py`, an LLP64 64-bit-truncation bug
> in the Sail runtime on Windows) are documented in the build steps below
> and in `vendor/README.md`.

---

## Quick start (no Sail toolchain, no system libraries)

The crate ships everything in `vendor/`: the pre-generated model
(`model.cpp.gz`, 18 MB, unpacked to `OUT_DIR` at build time) and a
**portability-patched** Sail C runtime with bundled mini-gmp — no GMP, no
zlib, no opam/Sail/z3. You only need C and C++ compilers (C++20: g++ 10+/
clang 10+/VS2019 16.11+):

```sh
cargo test               # works out of the box (Oracle is Send+Sync)
cargo nextest run        # …or process-per-test for real parallelism
```

This works unchanged on Linux (glibc/musl), macOS, and Windows — **both**
`x86_64-pc-windows-msvc` (verified: VS2022 cl.exe builds it and the tests
pass natively) and `x86_64-pc-windows-gnu` (verified via cross-compiled
smoke test). See `vendor/README.md` for what was patched and the mini-gmp
license note (LGPL, unlike the rest).

Optional env switches:

- `SAIL_SYSTEM_GMP=1` — keep the vendored runtime but link real libgmp
  (faster arbitrary-precision paths; needs gmp headers+lib).
- `SAIL_MODEL_C` / `SAIL_LIB_DIR` — build against a freshly generated model
  and/or an external (opam) runtime; that path needs system GMP + zlib like
  upstream Sail (on Alpine/musl see the static-linking note in Step 3).

Steps 1–2 below are **only** needed to regenerate `model.c` after editing
`sail/harness.sail` (or to track a newer sail-arm); refresh the vendored copy
afterwards as described in `vendor/README.md`.

---

## Prerequisites (for REGENERATING the model only)

- **OCaml + Sail** (the compiler), via opam:
  ```sh
  opam install sail
  ```
- **z3** on `PATH` — Sail's typechecker shells out to it and dies without it
  (`SMT solver returned unexpected status 127`). `apt install z3` /
  `apk add z3` / `brew install z3`. (No root? `apk fetch z3` + untar into
  `~/.local` and add `~/.local/usr/bin` to `PATH` works on Alpine.)
- **GMP** and **zlib** development libraries (the Sail C runtime needs them):
  - Debian/Ubuntu: `sudo apt install libgmp-dev zlib1g-dev`
  - macOS: `brew install gmp` (zlib ships with the SDK)
  - Windows: use the **MSYS2/MinGW** toolchain (`pacman -S mingw-w64-x86_64-gmp
    mingw-w64-x86_64-zlib`). MSVC + GMP is painful; MinGW is the smooth path.
- The Sail ARM model:
  ```sh
  git clone https://github.com/rems-project/sail-arm
  ```
- A C compiler and `cargo`. For concurrent testing: `cargo install cargo-nextest`.

---

## Step 1 — locate the Sail runtime lib dir

The generated C `#include`s `sail.h` and links against `sail.c`/`rts.c`/`elf.c`,
which ship with the `sail` opam package:

```sh
# Common location; adjust to your opam switch:
export SAIL_LIB_DIR="$(opam var share)/sail/lib"
ls "$SAIL_LIB_DIR"/sail.h "$SAIL_LIB_DIR"/rts.c   # sanity check
```

## Step 2 — generate the model C++ (with our harness)

The source list below is `SAIL_SRCS` from `sail-arm/arm-v9.4-a/Makefile`
(**without** `src/elfmain.sail` — that file provides the emulator `main`,
which `harness.sail` replaces). Our harness is appended **last**.

Flags that matter beyond the Makefile's own C target:

- `--cpp` — the C++ backend: emits `class model::Model` with all
  architectural state as instance members (this is what makes multiple
  independent oracles per process possible);
- `--c-no-main`;
- `--c-preserve <fn>` for **each** harness wrapper: since `main` is gone,
  nothing references them, and Sail's dead-code elimination silently drops
  them otherwise.

This is the exact command verified with Sail 0.20.1 (takes ~15 min and ~9 GB
of RAM; `--memo-z3` makes re-runs much faster), **including the mandatory
fixup script** (the cpp backend has several rough edges on a model this
size — see `scripts/fix_cpp_model.py`'s docstring):

```sh
cd sail-arm/arm-v9.4-a

sail --cpp --c-no-main -O --Oconstant-fold --memo-z3 \
    --c-preserve main \
    --c-preserve init_harness \
    --c-preserve set_gpr  --c-preserve get_gpr \
    --c-preserve set_pc   --c-preserve get_pc \
    --c-preserve set_nzcv --c-preserve get_nzcv \
    --c-preserve step_a64 \
    src/prelude.sail src/decode_start.sail src/builtins.sail src/v8_base.sail \
    src/stubs.sail src/interrupts.sail src/interface.sail src/devices.sail \
    src/impdefs.sail src/mem.sail src/sysregs_autogen.sail src/sysregs.sail \
    src/reset.sail src/instrs32.sail src/instrs64.sail src/instrs64_sve.sail \
    src/instrs64_sme.sail src/decode_end.sail src/fetch.sail \
    src/map_clauses.sail src/event_clauses.sail \
    /path/to/sail-aarch64-oracle/sail/harness.sail \
    -o cpp/model

# MANDATORY: fix the cpp backend's output (dedup declarations, generate
# model_missing.cpp, refcount the global scratch lifecycle):
python3 /path/to/sail-aarch64-oracle/scripts/fix_cpp_model.py \
    cpp/model.h cpp/model.cpp

# -> produces cpp/model.cpp (~166 MB) + cpp/model.h + cpp/model_missing.cpp
export SAIL_MODEL_C="$PWD/cpp/model.cpp"
```

Sanity check that DCE kept the wrappers (note `nzcv` mangles to `nzzcv`):

```sh
grep -cE '^\s+(unit|uint64_t) z(init_harness|set_gpr|get_gpr|set_pc|get_pc|set_nzzcv|get_nzzcv|step_a64)\(' cpp/model.h
# must print 8
```

Gotchas observed in practice:

- **RAM**: Sail on this model peaks around **9 GB**. With 16 GB total, do not
  run other big compiles concurrently or the OOM killer takes Sail down — and
  because of shell pipelines it can *look* like success (exit 137 swallowed by
  `| tail`). If the output didn't change, check `dmesg | grep -i oom`.
- `harness.sail` provides a stub Sail `main` — even with `--c-no-main` the
  backend emits a `model_main()` that references `zmain`, which otherwise
  breaks compilation.

## Step 3 — build & test

```sh
cd /path/to/sail-aarch64-oracle
export SAIL_MODEL_C=/path/to/sail-arm/arm-v9.4-a/cpp/model.cpp
# Do NOT set SAIL_LIB_DIR: the cpp-backend model needs the sint/sub_vec_int
# aliases that only exist in the patched vendored runtime. (SAIL_LIB_DIR
# remains available for plain `sail -c` models against an opam runtime.)

cargo build
cargo nextest run
```

`build.rs` also compiles `sail_failure.c` / `sail_config.c` / `cJSON.c` from
`SAIL_LIB_DIR` when present — Sail ≥ 0.18 moved `sail_assert` and the config
machinery there.

### Alpine / musl note

Rust's `x86_64-unknown-linux-musl` toolchain links **statically**, so the
linker wants `libgmp.a`/`libz.a`, not the `.so`s — install `gmp-static` and
`zlib-static` (or extract them with the rootless `apk fetch` trick) and point
the build at them:

```sh
export GMP_LIB_DIR=$HOME/.local/usr/lib    # contains libgmp.a
export ZLIB_LIB_DIR=$HOME/.local/usr/lib   # contains libz.a
export CFLAGS="-I$HOME/.local/usr/include" # zlib.h, if zlib-dev was extracted there
```

### Verifying accessor names (only if Step 2 fails to typecheck)

If `sail -c` errors inside `harness.sail`, find the real names in the model:

```sh
cd sail-arm/arm-v9.4-a
grep -RnE '^\s*(val|function)\s+(X|_PC|PC64|PSTATE|__DecodeA64|TakeReset|__InitSystem|__ResetState)\b' .
```

Then fix the matching wrapper in `sail/harness.sail` (GPR getter/setter arg
order, the PC register name, the decode signature, the reset entry point) and
re-run Step 2. This is the only place names can be wrong.

To confirm the *generated* C symbols (for debugging the link step):

```sh
nm -g model.c.o 2>/dev/null | grep -E 'zset_gpr|zstep_a64|model_init'
# or after building:
nm -g target/debug/build/*/out/libsailarm.a | grep zset_gpr
```

---

## Building on Windows 11

With the model and patched runtime vendored in `vendor/`, Windows needs **no
Sail and no GMP/zlib at all** — only a Rust toolchain and a C compiler. The
whole C side (model + runtime + mini-gmp + shim) cross-compiles cleanly with
x86_64-w64-mingw32-gcc. (If you regenerate the model, do it in WSL2/Linux:
Sail is OCaml-based and its Windows support is weak.)

Both Windows toolchains are verified on Windows 11:

- **MSVC** (`x86_64-pc-windows-msvc`, the rustup default): `cargo build`
  with VS2022 works out of the box — `build.rs` passes `/bigobj` (model.c
  exceeds the default COFF section limit) and `/std:c11`, and the runtime
  carries MSVC guards (getopt stub, `ssize_t`/`getline` fallbacks). Tests
  pass natively.
- **MinGW** (`x86_64-pc-windows-gnu`): a statically linked smoke test
  (model + patched runtime + mini-gmp + shim, built with
  x86_64-w64-mingw32-gcc) executes `ADD X0,X1,X2` correctly natively.

```powershell
# In MSYS2 (MinGW64):  pacman -S mingw-w64-x86_64-gcc   (only a C compiler!)
rustup target add x86_64-pc-windows-gnu

cargo +stable-x86_64-pc-windows-gnu nextest run --target x86_64-pc-windows-gnu
```

With the default MSVC toolchain it is simply:

```powershell
cargo test               # safe: Oracle is Send+Sync (mutex-serialized)
# or, for real process-level parallelism:
cargo install cargo-nextest
cargo nextest run
```

- **All-in-WSL2**: also works, but yields a Linux artifact — only use it if
  your whole test harness also runs in WSL.

## Concurrency

The model class (`sail --cpp`) keeps **all architectural state per instance**,
and the shim gives each instance **its own memory** (it swaps the instance's
`sail_memory`/`sail_tags` block lists into the runtime globals around every
model call):

- **any number of `Oracle`s per process** — fully independent CPUs, registers
  AND memory (verified by `two_oracles_are_independent` and
  `memory_is_per_instance`); construct, interleave and drop them freely;
- **`Oracle` is `Send + Sync`**: the Sail C *runtime* is not thread-safe
  (process-global GMP scratch), so every model call takes a process-wide
  mutex in the Rust wrapper. Cross-thread use is therefore safe by
  construction — moved instances, `Arc`-shared instances, concurrent test
  threads all just serialize (verified by `oracles_usable_across_threads`,
  and the whole suite runs under plain parallel `cargo test`);
- threads give you **safety, not speedup** (the lock serializes all model
  calls). For real parallelism use **process-per-test** via
  `cargo nextest run` — near-linear scaling, no shared lock.

With proptest, one `#[test]` per instruction; a fresh `Oracle` per case is
now perfectly fine (cheap relative to a proptest run, and gives you clean
reset-state semantics):

```rust
proptest! {
    #[test]
    fn add_matches_model(a in any::<u64>(), b in any::<u64>()) {
        let mut cpu = Oracle::new();   // independent instance per case
        cpu.set_x(1, a); cpu.set_x(2, b);
        cpu.step(0x8B020020);          // ADD X0, X1, X2
        prop_assert_eq!(cpu.get_x(0), my_model_add(a, b));
    }
}
```

Loads/stores work out of the box at reset state (EL3, MMU off, flat physical
mapping) — `memory_is_per_instance` exercises `STR`/`LDR` round-trips and
cross-instance isolation at `0x8000_0000`. Memory can also be seeded and
inspected **directly by address** (`write_mem_u64`/`read_mem_u64` and friends),
observing exactly what instructions see — `direct_memory_access_matches_instructions`
cross-checks the two paths. Reads of never-written addresses return 0 without
allocating; `is_mapped(addr)` tells "wrote 0" apart from "never touched"
(at the runtime's 16 MiB block granularity).

---

## Next steps (beyond this milestone)

1. **More state accessors, same recipe**: MTE tag-control sysregs
   (`GCR_EL1`/`RGSR_EL1`) if deterministic IRG is needed, pointer-auth keys
   (`AP*Key_EL1`), debug/breakpoint registers.
2. **FPSR in `CpuState`**: fold FP exception flags into the snapshot struct so
   the convenience diff path catches FP divergence, not just GPR/PC/NZCV.
3. **Newer than v9.4 (SME2 etc.)**: regenerate the Sail from newer ARM ASL via
   `rems-project/asl_to_sail` (needs the classic asl-interpreter / opam `asli`;
   note the maintained ASLi has moved to `IntelLabs/isa-tools` and may not be a
   drop-in for `asl_to_sail`).
