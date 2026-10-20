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

/// AESE V0.16B, V1.16B  (AES single round encrypt: SubBytes(ShiftRows(V0^V1)))
///   llvm-mc -mattr=+aes: [0x20,0x48,0x28,0x4e]
const AESE_V0_V1: u32 = 0x4E28_4820;

/// AES single-round encrypt. Reproduces a downstream differential-test case:
/// state=0, key=0x3d8e163fc. AESE has no MixColumns and AddRoundKey is FIRST, so
/// result = SubBytes(ShiftRows(state ^ key)); with state=0, = SubBytes(ShiftRows(key)).
/// Expected (AES-checked) q0 = 0x6363fb6363f863636163637b636363b0.
///
/// REGRESSION: an LLP64 bug in append_64() (mpz_add_ui truncates the 64-bit
/// chunk to 32 bits on Windows) zeroed the high 4 bytes of every 64-bit group in
/// large bit-vector constants — including the AES S-box — so S-box lookups with
/// index%8 >= 4 (e.g. 0xfc, 0xff) returned 0. Fixed in vendor/sail-runtime/sail.h.
#[test]
fn aese_single_round() {
    let mut cpu = Oracle::new();

    cpu.set_z(0, &[0x0, 0x0]); // state = 0
    cpu.set_z(1, &[0x3_d8e1_63fc, 0x0]); // key
    cpu.step(AESE_V0_V1);

    let z0 = cpu.get_z(0);
    assert_eq!(z0[0], 0x6163_637b_6363_63b0, "AESE low 64 (byte0 = S(0xfc) = 0xb0)");
    assert_eq!(z0[1], 0x6363_fb63_63f8_6363, "AESE high 64");

    // Exercise S-box lookups across all four byte-positions of a 64-bit group,
    // incl. the high half that the append_64 bug used to zero (0xfc, 0xfd, 0xff).
    // With state=0 and only key byte0 set, result byte0 = S-box(key_byte0).
    for (k0, want) in [
        (0x00u8, 0x63u8), (0x01, 0x7c), (0x53, 0xed), (0x7f, 0xd2),
        (0xf8, 0x41), (0xfc, 0xb0), (0xfd, 0x54), (0xfe, 0xbb), (0xff, 0x16),
    ] {
        cpu.set_z(0, &[0x0, 0x0]);
        cpu.set_z(1, &[k0 as u64, 0x0]);
        cpu.step(AESE_V0_V1);
        assert_eq!(cpu.get_z(0)[0] as u8, want, "AESE S-box({k0:#04x})");
    }
}

/// DC ZVA, X1  (sys #3, c7, c4, #1) — zero a DCZID_EL0.BS-sized aligned block.
const DC_ZVA_X1: u32 = 0xD50B_7421;
/// MRS X2, DCZID_EL0
const MRS_X2_DCZID: u32 = 0xD53B_00E2;

/// DC ZVA is implemented (DCZID_EL0.BS = 8 -> 1 KiB block). But in the oracle's
/// reset state the MMU is OFF (SCTLR.M=0), so every data access is Device-nGnRnE,
/// and the Arm ARM requires DC ZVA to a Device location to take an Alignment
/// fault. So with no MMU set up it FAULTS (Data Abort, DFSC=alignment) and zeroes
/// nothing — correct behavior, not a missing-instruction. (To actually zero, map
/// the target as Normal memory via the MMU first.)
#[test]
fn dc_zva_faults_on_device_memory() {
    const ADDR: u64 = 0x8000_0000;
    let mut cpu = Oracle::new();

    // DCZID_EL0 is readable: BS[3:0] = log2 block size in words; DZP[4]=0 (allowed).
    cpu.step(MRS_X2_DCZID);
    let dczid = cpu.get_x(2);
    assert_eq!(dczid & 0xf, 8, "DCZID_EL0.BS = 8 (1 KiB block)");
    assert_eq!((dczid >> 4) & 1, 0, "DZP=0: DC ZVA not prohibited at this EL");

    let fill = vec![0xAAu8; 0x800];
    cpu.write_mem(ADDR, &fill);
    cpu.set_x(1, ADDR + 0x40);
    cpu.set_pc(0x4000);
    cpu.step(DC_ZVA_X1);

    // Synchronous exception to EL3 (sync vector, current EL with SP_ELx).
    assert_eq!(cpu.get_pc() & 0x7ff, 0x200, "DC ZVA took a synchronous exception");
    let esr = cpu.esr_el3();
    assert_eq!((esr >> 26) & 0x3f, 0x25, "EC = Data Abort (current EL)");
    assert_eq!(esr & 0x3f, 0x21, "DFSC = Alignment fault (DC ZVA on Device memory)");
    assert_eq!(cpu.elr_el3(), 0x4000, "ELR_EL3 = faulting DC ZVA");

    // Nothing was zeroed: the pattern is intact.
    let mut buf = vec![0u8; 0x800];
    cpu.read_mem(ADDR, &mut buf);
    assert!(buf.iter().all(|&b| b == 0xAA), "memory untouched by the faulted DC ZVA");
}

/// With the MMU turned on and the target mapped as Normal memory, DC ZVA no
/// longer faults — it zeroes the DCZID block. There is NO direct API to set
/// SCTLR/TCR/MAIR/TTBR, so we configure the EL3 translation regime by stepping
/// the MSR sequence and seed the page table via write_mem.
#[test]
fn dc_zva_works_with_mmu_on() {
    const BUF: u64 = 0x8000_0000;
    const TT: u64 = 0x9000_0000; // L1 table base (lives inside the block we map)
    let mut cpu = Oracle::new();

    cpu.write_mem(BUF, &vec![0xAAu8; 0x800]); // nonzero pattern

    // Minimal identity map: 4KB granule, T0SZ=25 -> walk starts at L1, each L1
    // entry maps a 1 GiB block. VA 0x8000_0000 -> L1 index [38:30] = 2.
    // descriptor = base(0x8000_0000) | AF(1<<10) | SH=inner(3<<8) | AttrIndx=0 |
    //              block(0b01) = 0x8000_0701  (Normal memory via MAIR Attr0).
    cpu.write_mem_u64(TT + 2 * 8, 0x8000_0701);

    let setup: [u32; 11] = [
        0xD2801FE0, // mov  x0, #0xff
        0xD51EA200, // msr  MAIR_EL3, x0        ; Attr0 = Normal WB
        0xD286A320, // mov  x0, #0x3519
        0xF2A00040, // movk x0, #2, lsl #16     ; TCR_EL3 = 0x23519 (T0SZ=25,4KB,WB,IS,PS=40b)
        0xD51E2040, // msr  TCR_EL3, x0
        0xD2B20000, // mov  x0, #0x9000_0000
        0xD51E2000, // msr  TTBR0_EL3, x0
        0xD53E1000, // mrs  x0, SCTLR_EL3
        0xB2400000, // orr  x0, x0, #1          ; M = 1
        0xD51E1000, // msr  SCTLR_EL3, x0
        0xD5033FDF, // isb
    ];
    cpu.set_pc(0x4000);
    for op in setup {
        cpu.step(op);
    }

    cpu.set_x(1, BUF + 0x40);
    cpu.step(0xD50B_7421); // dc zva, x1

    assert_ne!(
        cpu.get_pc() & 0x7ff, 0x200,
        "DC ZVA still faulted (ESR_EL3={:#x}, DFSC={:#x})", cpu.esr_el3(), cpu.esr_el3() & 0x3f
    );

    let mut buf = vec![0u8; 0x800];
    cpu.read_mem(BUF, &mut buf);
    assert!(buf[..0x400].iter().all(|&b| b == 0), "1 KiB block must be zeroed");
    assert!(buf[0x400..].iter().all(|&b| b == 0xAA), "past the block must be untouched");
    println!("OK: MMU on -> DC ZVA zeroed [{BUF:#x}, {:#x}); no fault", BUF + 0x400);
}

/// LDTR x30,[x1,#5] (unprivileged load) vs LDUR x30,[x1,#5] — probe whether the
/// loaded 64-bit value loses its top 2 bits.
#[test]
fn ldtr_value_probe() {
    const A: u64 = 0x8000_0008; // x1+5; 8-aligned so a Device-memory load won't fault
    let mut cpu = Oracle::new();
    for v in [
        0xFFFF_FFFF_FFFF_FFFFu64,
        0xC000_0000_0000_0003,
        0x8000_0000_0000_0001,
        0x4000_0000_0000_0000,
        0x0123_4567_89AB_CDEF,
    ] {
        for (name, op) in [("LDTR", 0xF840_583Eu32), ("LDUR", 0xF840_503E)] {
            cpu.write_mem_u64(A, v);
            cpu.set_x(1, A - 5);
            cpu.set_x(30, 0);
            cpu.set_pc(0x4000);
            cpu.step(op);
            let got = cpu.get_x(30);
            let trap = cpu.get_pc() & 0x7ff == 0x200;
            println!("{name} v={v:#018x} -> x30={got:#018x}  top2={:02b}  trap={trap}", got >> 62);
        }
    }
}

/// UNALIGNED LDTR with the MMU on (Normal memory): x1 aligned, addr = x1+5 ≡ 5
/// mod 8, so the 8-byte load is unaligned and goes through the byte-assembly path.
#[test]
fn ldtr_unaligned_with_mmu() {
    const BUF: u64 = 0x8000_0000;
    const TT: u64 = 0x9000_0000;
    let mut cpu = Oracle::new();

    // 0xFF across the window so an unaligned 8-byte load should read all-ones.
    cpu.write_mem(BUF, &vec![0xFFu8; 0x40]);
    cpu.write_mem_u64(TT + 2 * 8, 0x8000_0701); // Normal-memory L1 block

    let setup: [u32; 11] = [
        0xD2801FE0, 0xD51EA200, 0xD286A320, 0xF2A00040, 0xD51E2040, 0xD2B20000,
        0xD51E2000, 0xD53E1000, 0xB2400000, 0xD51E1000, 0xD5033FDF,
    ];
    cpu.set_pc(0x4000);
    for op in setup {
        cpu.step(op);
    }

    cpu.set_x(1, BUF); // x1 aligned -> addr = BUF+5 is unaligned
    cpu.set_x(30, 0);
    cpu.step(0xF840_583E); // ldtr x30, [x1, #5]
    let got = cpu.get_x(30);
    let trap = cpu.get_pc() & 0x7ff == 0x200;
    println!("UNALIGNED LDTR @ {:#x} -> x30={got:#018x}  top2={:02b}  trap={trap}", BUF + 5, got >> 62);
}

/// Reproduce the failing unaligned 32-bit LDTR at a low address. Each byte is
/// set to its own low address byte, so the loaded word decodes to exact source
/// addresses. x1=0x1f218, addr=x1+5=0x1f21d (unaligned). MMU on, [0,1GiB) Normal.
#[test]
fn ldtr_w2_unaligned_low_addr() {
    const TT: u64 = 0x9000_0000;
    let mut cpu = Oracle::new();

    // Fill 0x1f200..0x1f240 with byte[A] = A & 0xff.
    let region: Vec<u8> = (0..0x40u64).map(|i| ((0x1f200 + i) & 0xff) as u8).collect();
    cpu.write_mem(0x1f200, &region);
    // API sanity: unaligned u64/u32 read at 0x1f21d must reflect what we wrote.
    println!("API read_mem_u64(0x1f21d) = {:#018x} (expect ..201f1e1d)", cpu.read_mem_u64(0x1f21d));

    // Identity-map [0,1GiB) as Normal so the unaligned load doesn't fault.
    cpu.write_mem_u64(TT + 0 * 8, 0x0000_0701); // L1 idx 0 covers 0x1f218
    let setup: [u32; 11] = [
        0xD2801FE0, 0xD51EA200, 0xD286A320, 0xF2A00040, 0xD51E2040, 0xD2B20000,
        0xD51E2000, 0xD53E1000, 0xB2400000, 0xD51E1000, 0xD5033FDF,
    ];
    cpu.set_pc(0x4000);
    for op in setup {
        cpu.step(op);
    }

    cpu.set_x(1, 0x1f218);
    cpu.set_x(2, 0);
    cpu.step(0xB840_5822); // ldtr w2, [x1, #5]
    let w2 = cpu.get_x(2) as u32;
    let trap = cpu.get_pc() & 0x7ff == 0x200;
    println!(
        "LDTR w2,[x1,#5] addr=0x1f21d -> w2={w2:#010x} bytes={:02x?} (expect 0x201f1e1d) trap={trap}",
        w2.to_le_bytes()
    );
}

/// Reproduce the user's exact case: default config (MMU off), x1=0x1f218, no data
/// written. addr = x1+5 = 0x1f21d is UNALIGNED for a 4-byte load; MMU-off memory
/// is Device-nGnRnE, and unaligned Device access faults -> the load never runs and
/// w2 keeps its INPUT value (here the sentinel 0x2657d469).
#[test]
fn ldtr_unaligned_device_faults() {
    let mut cpu = Oracle::new();
    cpu.set_x(1, 0x1f218);
    cpu.set_x(2, 0x2657_d469); // sentinel = "input" value of w2
    cpu.set_pc(0x4000);
    cpu.step(0xB840_5822); // ldtr w2, [x1, #5]
    let esr = cpu.esr_el3();
    println!(
        "PC={:#x} trap={} EC={:#x} DFSC={:#x} x2={:#x}",
        cpu.get_pc(), cpu.get_pc() & 0x7ff == 0x200, (esr >> 26) & 0x3f, esr & 0x3f, cpu.get_x(2)
    );
    // Aligned control: x1+5 made 4-aligned (x1=0x1f213 -> addr 0x1f218) loads fine.
    cpu.set_x(1, 0x1f213);
    cpu.set_x(2, 0x2657_d469);
    cpu.write_mem_u64(0x1f218, 0x0000_0000_AABB_CCDD);
    cpu.set_pc(0x4000);
    cpu.step(0xB840_5822);
    println!(
        "aligned: PC={:#x} trap={} x2={:#x} (expect 0xaabbccdd)",
        cpu.get_pc(), cpu.get_pc() & 0x7ff == 0x200, cpu.get_x(2)
    );
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

/// FRECPE S0, S1  (scalar single-precision reciprocal estimate).
/// Verified encoding: `llvm-mc -triple=aarch64` -> [0x20,0xd8,0xa1,0x5e].
const FRECPE_S0_S1: u32 = 0x5EA1_D820;
/// FPCR.AH (Alternate Handling) — bit[1]. Requires FEAT_AFP (implemented).
const FPCR_AH: u64 = 1 << 1;

/// Runs FRECPE S0,S1 on `x` and returns (S0_result_f32, FPSR_after).
fn frecpe_f32(cpu: &mut Oracle, x: f32) -> (f32, u64) {
    cpu.set_z(1, &[x.to_bits() as u64]); // S1 = low 32 bits of Z1
    cpu.set_fpsr(0);
    cpu.step(FRECPE_S0_S1);
    (f32::from_bits(cpu.get_z(0)[0] as u32), cpu.get_fpsr())
}

/// FEAT_RPRES gives FRECPE/FRSQRTE a 12-bit (vs the legacy 8-bit) mantissa
/// estimate, but ONLY for single precision AND only when FPCR.AH = 1 (the
/// "alternate FP handling" path, FEAT_AFP). Under AH=1 the estimate also takes
/// the exception-free path (fpexc = false), so FPSR is left untouched.
///
/// This test pins both facts by running the same FRECPE with AH=0 and AH=1:
/// the AH=1 (RPRES) result is a strictly tighter approximation of 1/x, and it
/// accumulates nothing into FPSR.
#[test]
fn frecpe_rpres_increased_precision_under_ah() {
    let mut cpu = Oracle::new();
    let x = 3.0f32;
    let truth = (1.0f64) / (x as f64); // reference reciprocal in f64

    // AH=0: legacy 8-bit estimate.
    cpu.set_fpcr(0);
    let (est8, _) = frecpe_f32(&mut cpu, x);

    // AH=1: RPRES 12-bit estimate (and exception-free).
    cpu.set_fpcr(FPCR_AH);
    let (est12, fpsr_after) = frecpe_f32(&mut cpu, x);

    let rel = |e: f32| ((e as f64) - truth).abs() / truth;
    let (err8, err12) = (rel(est8), rel(est12));

    // 1) The two precisions genuinely differ for this input.
    assert_ne!(
        est8.to_bits(),
        est12.to_bits(),
        "AH=1 must select a different (12-bit) estimate than AH=0"
    );
    // 2) The RPRES estimate is strictly closer to the true reciprocal.
    assert!(
        err12 < err8,
        "RPRES estimate must be tighter: err12={err12:e} !< err8={err8:e}"
    );
    // 3) ...and lands well inside the 12-bit relative-error class (~2^-12),
    //    which the 8-bit estimate (~2^-8) cannot reach.
    assert!(
        err12 < 2.0f64.powi(-11),
        "RPRES estimate should be ~12-bit accurate: err12={err12:e}"
    );
    assert!(
        err8 > 2.0f64.powi(-11),
        "legacy estimate should be ~8-bit accurate: err8={err8:e}"
    );
    // 4) AH=1 path is exception-free: FPSR untouched (fpexc = false).
    assert_eq!(fpsr_after, 0, "FRECPE under AH=1 must not update FPSR");
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
