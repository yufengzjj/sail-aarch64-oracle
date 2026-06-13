//! End-to-end smoke tests: drive real A64 instructions through the Sail model.
//!
//! The model is a C++ class instance — every `Oracle::new()` is an
//! independent CPU, so tests can freely create several. `Oracle` is
//! `Send + Sync` (a process-wide mutex serializes model calls), so plain
//! parallel `cargo test` is safe; `cargo nextest run` gives real
//! process-level parallelism.

use sail_aarch64_oracle::{Oracle, P_CHUNKS, Z_CHUNKS, ZT0_CHUNKS};

/// FADD D0, D1, D2  (scalar double)
const FADD_D0_D1_D2: u32 = 0x1E62_2820;
/// FDIV D3, D4, D5  (scalar double)
const FDIV_D3_D4_D5: u32 = 0x1E65_1883;
/// SMSTART (MSR SVCRSMZA, #1): enter streaming mode + enable ZA
const SMSTART: u32 = 0xD503_477F;
/// ZERO {ZA}: zero the whole ZA storage
const ZERO_ZA: u32 = 0xC008_00FF;
/// FPSR.DZC — divide-by-zero cumulative flag
const FPSR_DZC: u64 = 1 << 1;

/// ADD X0, X1, X2   (64-bit, shifted-register form, no shift)
///   sf=1 .0001011 shift=00 .0 Rm Rn Rd  ->  0x8B020020 for Rm=2,Rn=1,Rd=0
const ADD_X0_X1_X2: u32 = 0x8B02_0020;

#[test]
fn add_two_registers() {
    let mut cpu = Oracle::new();

    cpu.set_x(1, 3);
    cpu.set_x(2, 4);
    cpu.set_pc(0x1000);

    cpu.step(ADD_X0_X1_X2);

    assert_eq!(cpu.get_x(0), 7, "X0 should be X1 + X2 = 7");
    // Inputs must be untouched.
    assert_eq!(cpu.get_x(1), 3);
    assert_eq!(cpu.get_x(2), 4);
}

/// SUB X3, X4, X5  ->  0xCB050083  (sf=1 1001011 ... Rm=5,Rn=4,Rd=3), with flags untouched.
const SUB_X3_X4_X5: u32 = 0xCB05_0083;

#[test]
fn sub_two_registers() {
    let mut cpu = Oracle::new();

    cpu.set_x(4, 10);
    cpu.set_x(5, 4);

    cpu.step(SUB_X3_X4_X5);

    assert_eq!(cpu.get_x(3), 6, "X3 should be X4 - X5 = 6");
}

/// Two instances in one process must be completely independent: interleave
/// writes and steps and check neither leaks into the other.
#[test]
fn two_oracles_are_independent() {
    let mut a = Oracle::new();
    let mut b = Oracle::new();

    a.set_x(1, 100);
    a.set_x(2, 1);
    b.set_x(1, 200);
    b.set_x(2, 2);
    a.set_pc(0x1000);
    b.set_pc(0x2000);

    // Interleaved execution.
    a.step(ADD_X0_X1_X2);
    b.step(ADD_X0_X1_X2);

    assert_eq!(a.get_x(0), 101, "instance A: X0 = 100 + 1");
    assert_eq!(b.get_x(0), 202, "instance B: X0 = 200 + 2");
    assert_eq!(a.get_x(1), 100, "A's inputs untouched by B");
    assert_eq!(b.get_x(1), 200, "B's inputs untouched by A");
    assert_eq!(a.get_pc(), 0x1004, "A's PC advanced independently");
    assert_eq!(b.get_pc(), 0x2004, "B's PC advanced independently");

    // Dropping one instance must not affect the survivor (refcounted runtime).
    drop(a);
    b.set_x(4, 50);
    b.set_x(5, 8);
    b.step(SUB_X3_X4_X5);
    assert_eq!(b.get_x(3), 42, "B still works after A is dropped");
}

/// STR X0, [X1]  (64-bit, unsigned offset 0)
const STR_X0_X1: u32 = 0xF900_0020;
/// LDR X2, [X1]  (64-bit, unsigned offset 0)
const LDR_X2_X1: u32 = 0xF940_0022;

/// Each instance owns its own memory: a store in one must not be visible in
/// the other, and must read back correctly in its own instance.
#[test]
fn memory_is_per_instance() {
    const ADDR: u64 = 0x8000_0000; // RAM; MMU is off after reset (flat mapping)

    let mut a = Oracle::new();
    let mut b = Oracle::new();

    a.set_x(0, 0xDEAD_BEEF_CAFE_F00D);
    a.set_x(1, ADDR);
    a.step(STR_X0_X1);

    // A reads back its own store.
    a.set_x(2, 0);
    a.step(LDR_X2_X1);
    assert_eq!(a.get_x(2), 0xDEAD_BEEF_CAFE_F00D, "A sees its own store");

    // B reads the same physical address: fresh (zero) memory, not A's data.
    b.set_x(1, ADDR);
    b.set_x(2, 0x1111_1111_1111_1111);
    b.step(LDR_X2_X1);
    assert_eq!(b.get_x(2), 0, "B must not see A's store");

    // ...and B's own store doesn't leak back into A.
    b.set_x(0, 0x0BAD_0BAD_0BAD_0BAD);
    b.step(STR_X0_X1);
    a.step(LDR_X2_X1);
    assert_eq!(a.get_x(2), 0xDEAD_BEEF_CAFE_F00D, "A still sees its value");
}

/// Direct byte-addressed memory access agrees with what instructions see:
/// a value seeded via the API is loaded by LDR, and a value stored by STR is
/// read back through the API. Confirms little-endian byte order both ways.
#[test]
fn direct_memory_access_matches_instructions() {
    const ADDR: u64 = 0x8000_0000;

    let mut cpu = Oracle::new();

    // Seed memory directly, then have an instruction load it.
    cpu.write_mem_u64(ADDR, 0x0123_4567_89AB_CDEF);
    cpu.set_x(1, ADDR);
    cpu.step(LDR_X2_X1);
    assert_eq!(cpu.get_x(2), 0x0123_4567_89AB_CDEF, "LDR sees API-written value");

    // Little-endian: byte at ADDR is the LSB.
    assert_eq!(cpu.read_mem_byte(ADDR), 0xEF);
    assert_eq!(cpu.read_mem_byte(ADDR + 7), 0x01);

    // Now store via an instruction and read it back through the API.
    cpu.set_x(0, 0xFEED_FACE_DEAD_C0DE);
    cpu.step(STR_X0_X1);
    assert_eq!(cpu.read_mem_u64(ADDR), 0xFEED_FACE_DEAD_C0DE, "API sees STR value");

    // Unwritten memory reads back as zero.
    assert_eq!(cpu.read_mem_u64(0x9000_0000), 0);

    // is_mapped distinguishes "wrote 0" from "never touched": both read 0.
    assert!(cpu.is_mapped(ADDR), "written region is mapped");
    assert!(!cpu.is_mapped(0xA000_0000), "untouched region is not mapped");
    cpu.write_mem_u64(0xA000_0000, 0);
    assert_eq!(cpu.read_mem_u64(0xA000_0000), 0);
    assert!(cpu.is_mapped(0xA000_0000), "explicit zero write maps the region");

    // Block read/write round-trips.
    let bytes = [1u8, 2, 3, 4, 5, 6, 7, 8];
    cpu.write_mem(ADDR + 0x100, &bytes);
    let mut out = [0u8; 8];
    cpu.read_mem(ADDR + 0x100, &mut out);
    assert_eq!(out, bytes);
}

/// ADD Z0.D, Z1.D, Z2.D  (SVE unpredicated vector add, 64-bit elements)
///   00000100 size=11 1 Zm=00010 000000 Zn=00001 Zd=00000
const ADD_Z0_Z1_Z2_D: u32 = 0x04E2_0020;
/// PTRUE P0.B  (all elements active)
const PTRUE_P0_B: u32 = 0x2518_E3E0;
/// BFMMLA Z0.S, Z1.H, Z2.H  (BF16 2x2 matrix multiply-accumulate, FEAT_BF16)
///   01100100 011 Zm=00010 111001 Zn=00001 Zda=00000
const BFMMLA_Z0_Z1_Z2: u32 = 0x6462_E420;

#[test]
fn sve_add_vectors() {
    let mut cpu = Oracle::new();
    assert_eq!(cpu.vl_bits(), 2048, "init_harness requests max VL");

    // Distinct value per 64-bit lane across the full vector.
    let z1: Vec<u64> = (0..Z_CHUNKS as u64).map(|i| 0x1000 + i).collect();
    let z2: Vec<u64> = (0..Z_CHUNKS as u64).map(|i| 0x2000_0000 + 3 * i).collect();
    cpu.set_z(1, &z1);
    cpu.set_z(2, &z2);

    cpu.step(ADD_Z0_Z1_Z2_D);

    let z0 = cpu.get_z(0);
    for i in 0..Z_CHUNKS {
        assert_eq!(z0[i], z1[i] + z2[i], "Z0 lane {i} = Z1 + Z2");
    }
    // Sources untouched.
    assert_eq!(cpu.get_z(1).as_slice(), z1.as_slice());
    assert_eq!(cpu.get_z(2).as_slice(), z2.as_slice());
}

/// BF16 matrix-multiply-accumulate: the heaviest exact-rational FP path in the
/// model (per 128-bit segment: 2x4 . 4x2 BF16 dot products into 2x2 fp32). With
/// every BF16 input = 1.0 and the accumulator zeroed, each output fp32 lane is
/// the dot product of a row of ones with a column of ones = 1+1+1+1 = 4.0,
/// exactly, in every one of the 16 segments at VL=2048.
#[test]
fn sve_bf16_matmul() {
    let mut cpu = Oracle::new();
    assert_eq!(cpu.vl_bits(), 2048, "init_harness requests max VL");

    // BF16 1.0 = 0x3F80 (top 16 bits of fp32 1.0); four per 64-bit chunk.
    let ones_bf16 = vec![0x3F80_3F80_3F80_3F80u64; Z_CHUNKS];
    cpu.set_z(1, &ones_bf16);
    cpu.set_z(2, &ones_bf16);
    cpu.set_z(0, &[0u64; Z_CHUNKS]); // accumulator must start at 0

    cpu.step(BFMMLA_Z0_Z1_Z2);

    // fp32 4.0 = 0x40800000; two lanes per 64-bit chunk, all chunks identical.
    let z0 = cpu.get_z(0);
    for i in 0..Z_CHUNKS {
        assert_eq!(z0[i], 0x4080_0000_4080_0000, "Z0 chunk {i} = two fp32 4.0");
    }
    // Sources untouched.
    assert_eq!(cpu.get_z(1).as_slice(), ones_bf16.as_slice());
    assert_eq!(cpu.get_z(2).as_slice(), ones_bf16.as_slice());
}

// Benchmark: FCLAMP Zd.<T>, Zn.<T>, Zm.<T> (Zd = clamp(Zd, Zn..Zm)). FCLAMP is
// destructive, so Zd is refreshed with fresh RANDOM normal-float operands every
// iteration (otherwise it converges to a constant and re-clamps the same inputs
// forever). The per-iteration set_z refresh is timed on its own and subtracted,
// leaving FCLAMP-only time. Operands are normal floats with wide, varied
// exponents (no Inf/NaN/denormal fast/slow special-cases). Run:
//   cargo test --release --test single_instruction bench_fclamp -- --ignored --nocapture
#[test]
#[ignore = "benchmark"]
fn bench_fclamp() {
    const N: u32 = 2000;
    const POOL: usize = 64;

    fn xorshift(s: &mut u64) -> u64 {
        let mut x = *s;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *s = x;
        x
    }
    // Z_CHUNKS u64s packed with random NORMAL floats (ebits exp, mbits mantissa).
    fn rand_vec(seed: u64, ebits: u32, mbits: u32) -> [u64; Z_CHUNKS] {
        let mut s = seed | 1;
        let width = 1 + ebits + mbits;
        let lanes = 64 / width;
        let emax = (1u64 << ebits) - 1;
        let mut out = [0u64; Z_CHUNKS];
        for chunk in out.iter_mut() {
            let mut w = 0u64;
            for l in 0..lanes {
                let r = xorshift(&mut s);
                let sign = (r >> 63) & 1;
                let exp = emax / 4 + (r % (emax / 2)); // mid-range -> always normal
                let mant = r & ((1u64 << mbits) - 1);
                let elem = (sign << (ebits + mbits)) | (exp << mbits) | mant;
                w |= elem << (l as u32 * width);
            }
            *chunk = w;
        }
        out
    }

    // (name, opcode, exp_bits, mant_bits, lo=-100.0, hi=+100.0 packed per lane).
    let variants = [
        ("fclamp.h", 0x6462_2420u32, 5u32, 10u32, 0xD640_D640_D640_D640u64, 0x5640_5640_5640_5640u64),
        ("fclamp.s", 0x64A2_2420, 8, 23, 0xC2C8_0000_C2C8_0000, 0x42C8_0000_42C8_0000),
        ("fclamp.d", 0x64E2_2420, 11, 52, 0xC059_0000_0000_0000, 0x4059_0000_0000_0000),
    ];
    for vl in [128u32, 512, 2048] {
        for (name, op, eb, mb, lo, hi) in variants {
            let mut cpu = Oracle::new();
            let eff = cpu.set_vl(vl);
            cpu.set_z(1, &[lo; Z_CHUNKS]);
            cpu.set_z(2, &[hi; Z_CHUNKS]);
            let pool: Vec<[u64; Z_CHUNKS]> = (0..POOL)
                .map(|k| rand_vec(0x9E37_79B9_7F4A_7C15u64.wrapping_mul(k as u64 + 1) ^ op as u64, eb, mb))
                .collect();

            cpu.set_z(0, &pool[0]);
            cpu.set_pc(0x4000);
            cpu.step(op);
            assert_ne!(cpu.get_pc() & 0x7FF, 0x200, "{name} ({op:#010x}) trapped");

            // refresh-only baseline, then refresh + FCLAMP; net = FCLAMP alone.
            let t0 = std::time::Instant::now();
            for i in 0..N as usize {
                cpu.set_z(0, &pool[i % POOL]);
            }
            let refresh = t0.elapsed();
            let t1 = std::time::Instant::now();
            for i in 0..N as usize {
                cpu.set_z(0, &pool[i % POOL]);
                cpu.step(op);
            }
            let net = t1.elapsed().saturating_sub(refresh);
            println!(
                "{name} @ VL={eff:>4}: {:>8.2} µs/instr (net of {:.2}µs/it refresh)",
                net.as_nanos() as f64 / 1000.0 / N as f64,
                refresh.as_nanos() as f64 / 1000.0 / N as f64
            );
        }
    }
}

/// Runtime VL change: programming ZCR_EL3.LEN (via set_vl) is observable and
/// makes the SVE element count shrink, without disturbing GPRs or PC.
#[test]
fn vector_length_is_runtime_settable() {
    let mut cpu = Oracle::new();
    assert_eq!(cpu.vl_bits(), 2048, "reset requests max VL");

    // Scratch GPR + PC must survive a set_vl.
    cpu.set_x(0, 0xDEAD_BEEF_0000_0001);
    cpu.set_pc(0x8000_0000);

    for (req, expect) in [(128u32, 128u64), (256, 256), (512, 512), (1024, 1024), (2048, 2048)] {
        let eff = cpu.set_vl(req);
        assert_eq!(eff, expect, "set_vl({req}) -> {expect} b");
        assert_eq!(cpu.vl_bits(), expect);
    }

    assert_eq!(cpu.get_x(0), 0xDEAD_BEEF_0000_0001, "X0 preserved across set_vl");
    assert_eq!(cpu.get_pc(), 0x8000_0000, "PC preserved across set_vl");

    // At VL=256 b a .D vector has 256/64 = 4 active lanes; lanes above that are
    // not written by an unpredicated 64-bit ADD.
    cpu.set_vl(256);
    let z1: Vec<u64> = (0..Z_CHUNKS as u64).map(|i| 0x100 + i).collect();
    let z2: Vec<u64> = (0..Z_CHUNKS as u64).map(|i| 0x200 + i).collect();
    cpu.set_z(0, &vec![0u64; Z_CHUNKS]);
    cpu.set_z(1, &z1);
    cpu.set_z(2, &z2);
    cpu.step(ADD_Z0_Z1_Z2_D);
    let z0 = cpu.get_z(0);
    for i in 0..4 {
        assert_eq!(z0[i], z1[i] + z2[i], "lane {i} active at VL=256");
    }
    for i in 4..Z_CHUNKS {
        assert_eq!(z0[i], 0, "lane {i} inactive at VL=256");
    }
}

#[test]
fn sve_predicates() {
    let mut cpu = Oracle::new();

    // P register write/read roundtrip through the chunk ABI.
    let pat = [
        0xAAAA_AAAA_AAAA_AAAA,
        0x5555_5555_5555_5555,
        0xFFFF_0000_FFFF_0000,
        0x0123_4567_89AB_CDEF,
    ];
    cpu.set_p(3, &pat);
    assert_eq!(cpu.get_p(3), pat, "P3 chunk roundtrip");

    // PTRUE P0.B: with VL=2048 every byte lane is active -> all 256
    // predicate bits set.
    cpu.step(PTRUE_P0_B);
    assert_eq!(cpu.get_p(0), [u64::MAX; P_CHUNKS], "PTRUE sets all lanes");
    // P3 untouched by the PTRUE.
    assert_eq!(cpu.get_p(3), pat);
}

#[test]
fn fp_scalar_ops() {
    let mut cpu = Oracle::new();

    // FPCR roundtrip: select round-towards-zero (RMode = 0b11, bits 23:22).
    cpu.set_fpcr(0b11 << 22);
    assert_eq!(cpu.get_fpcr(), 0b11 << 22, "FPCR roundtrip");
    cpu.set_fpcr(0); // back to round-to-nearest for the ops below

    // D registers are the low 64 bits of the Z registers (chunk 0).
    cpu.set_z(1, &[1.5f64.to_bits()]);
    cpu.set_z(2, &[2.25f64.to_bits()]);
    cpu.step(FADD_D0_D1_D2);
    assert_eq!(
        cpu.get_z(0)[0],
        3.75f64.to_bits(),
        "FADD D0 = 1.5 + 2.25 (exact)"
    );

    // Divide by zero: result +Inf, FPSR.DZC set (and only DZC).
    cpu.set_fpsr(0);
    cpu.set_z(4, &[1.0f64.to_bits()]);
    cpu.set_z(5, &[0.0f64.to_bits()]);
    cpu.step(FDIV_D3_D4_D5);
    assert_eq!(cpu.get_z(3)[0], f64::INFINITY.to_bits(), "1.0/0.0 = +Inf");
    assert_eq!(cpu.get_fpsr(), FPSR_DZC, "FPSR: exactly DZC accumulated");
}

#[test]
fn sme_za_and_streaming() {
    let mut cpu = Oracle::new();

    assert_eq!(cpu.svcr(), 0, "SM=0, ZA=0 after reset");

    // SMSTART: PSTATE.SM=1 + PSTATE.ZA=1 (with mode-switch side effects).
    cpu.step(SMSTART);
    assert_eq!(cpu.svcr(), 0b11, "SM=1, ZA=1 after SMSTART");
    assert_eq!(cpu.vl_bits(), 2048, "SVL is max (SMCR_EL3.LEN)");

    // ZA storage roundtrip through the chunk ABI (after SMSTART, which
    // architecturally zeroes ZA on enable).
    let row0: Vec<u64> = (0..Z_CHUNKS as u64).map(|i| 0xA000_0000 + i).collect();
    let row255: Vec<u64> = (0..Z_CHUNKS as u64).map(|i| 0xB000_0000 + i).collect();
    cpu.set_za_row(0, &row0);
    cpu.set_za_row(255, &row255);
    assert_eq!(cpu.get_za_row(0).as_slice(), row0.as_slice(), "ZA[0] roundtrip");
    assert_eq!(cpu.get_za_row(255).as_slice(), row255.as_slice(), "ZA[255] roundtrip");

    // ZERO {ZA} wipes the whole storage.
    cpu.step(ZERO_ZA);
    assert_eq!(cpu.get_za_row(0), [0u64; Z_CHUNKS], "ZA[0] zeroed");
    assert_eq!(cpu.get_za_row(255), [0u64; Z_CHUNKS], "ZA[255] zeroed");
}

/// ADD X0, SP, #16  (uses SP as Rn=31 in the immediate form)
const ADD_X0_SP_16: u32 = 0x9100_43E0;
/// MRS X1, TPIDR_EL0
const MRS_X1_TPIDR: u32 = 0xD53B_D041;
/// MSR TPIDR_EL0, X2
const MSR_TPIDR_X2: u32 = 0xD51B_D042;
/// RDFFR P1.B
const RDFFR_P1: u32 = 0x2519_F001;

#[test]
fn sp_pstate_and_thread_pointers() {
    let mut cpu = Oracle::new();

    // Reset state: EL3 (pstate[12:11] = 0b11), SPSel = 1.
    let ps = cpu.get_pstate();
    assert_eq!((ps >> 11) & 0b11, 0b11, "running at EL3");
    assert_eq!((ps >> 13) & 1, 1, "SPSel = 1 (SP_EL3)");

    // SP is architecturally visible to instructions.
    cpu.set_sp(0x9000_0000);
    cpu.step(ADD_X0_SP_16);
    assert_eq!(cpu.get_x(0), 0x9000_0010, "X0 = SP + 16");
    assert_eq!(cpu.get_sp(), 0x9000_0000, "SP unchanged");

    // DAIF bits roundtrip through the packed pstate word.
    cpu.set_pstate(ps | 0b1111); // set D,A,I,F
    assert_eq!(cpu.get_pstate() & 0b1111, 0b1111, "DAIF set");
    cpu.set_pstate(ps & !0b1111);

    // Thread pointers via MRS/MSR.
    cpu.set_tpidr_el0(0x1122_3344_5566_7788);
    cpu.step(MRS_X1_TPIDR);
    assert_eq!(cpu.get_x(1), 0x1122_3344_5566_7788, "MRS reads TPIDR_EL0");

    cpu.set_x(2, 0xAA55);
    cpu.step(MSR_TPIDR_X2);
    assert_eq!(cpu.get_tpidr_el0(), 0xAA55, "MSR writes TPIDR_EL0");
}

#[test]
fn ffr_and_zt0() {
    let mut cpu = Oracle::new();

    // FFR roundtrip + RDFFR makes it architecturally visible in a predicate.
    let pat = [0xF0F0_F0F0_F0F0_F0F0u64, 0x0F0F_0F0F_0F0F_0F0F, u64::MAX, 0];
    cpu.set_ffr(&pat);
    assert_eq!(cpu.get_ffr(), pat, "FFR chunk roundtrip");
    cpu.step(RDFFR_P1);
    assert_eq!(cpu.get_p(1), pat, "RDFFR copies FFR into P1");

    // ZT0 (SME2) storage roundtrip.
    let zt: Vec<u64> = (0..ZT0_CHUNKS as u64).map(|i| 0xC0DE_0000 + i).collect();
    cpu.set_zt0(&zt);
    assert_eq!(cpu.get_zt0().as_slice(), zt.as_slice(), "ZT0 roundtrip");
}

/// Oracle is Send + Sync (every model call serialized by a process-wide
/// mutex): hammer two instances from two threads — interleaved creation,
/// stepping, reads and drops must neither crash nor cross-contaminate.
#[test]
fn oracles_usable_across_threads() {
    let a = std::thread::spawn(|| {
        let mut cpu = Oracle::new();
        for i in 0..100u64 {
            cpu.set_x(1, i);
            cpu.set_x(2, 1000);
            cpu.step(ADD_X0_X1_X2);
            assert_eq!(cpu.get_x(0), i + 1000, "thread A iteration {i}");
        }
        cpu.snapshot()
    });
    let b = std::thread::spawn(|| {
        let mut cpu = Oracle::new();
        for i in 0..100u64 {
            cpu.set_x(4, 5000 + i);
            cpu.set_x(5, i);
            cpu.step(SUB_X3_X4_X5);
            assert_eq!(cpu.get_x(3), 5000, "thread B iteration {i}");
        }
        cpu.snapshot()
    });
    let sa = a.join().expect("thread A");
    let sb = b.join().expect("thread B");
    assert_eq!(sa.x[0], 1099);
    assert_eq!(sb.x[3], 5000);

    // Sharing one instance between threads also compiles and is safe (Sync).
    let shared = std::sync::Arc::new(Oracle::new());
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let o = shared.clone();
            std::thread::spawn(move || {
                for _ in 0..50 {
                    assert_eq!(o.get_x(0), 0);
                    let _ = o.get_pstate();
                }
            })
        })
        .collect();
    for r in readers {
        r.join().expect("reader thread");
    }
}

#[test]
fn undefined_instruction_observability() {
    let mut cpu = Oracle::new();

    cpu.set_pc(0x4000);
    cpu.step(0x0000_0000); // UDF #0

    // The model vectors to EL3: ELR_EL3 records the faulting PC, ESR_EL3.EC
    // = 0 (unknown reason), and PC lands at VBAR_EL3 + 0x200 (sync, current
    // EL with SP_ELx).
    assert_eq!(cpu.elr_el3(), 0x4000, "ELR_EL3 = faulting PC");
    assert_eq!((cpu.esr_el3() >> 26) & 0x3F, 0, "ESR_EL3.EC = unknown");
    assert_eq!(cpu.get_pc() & 0x7FF, 0x200, "PC at sync vector offset");
}
