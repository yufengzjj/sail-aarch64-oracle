//! FEAT_MOPS prologue/main/epilogue sequences. Each case is its own test
//! because a broken block-size choice kills the whole process, not one test.
//! Only memory is checked where register results depend on IMP DEF choices.

use sail_aarch64_oracle::Oracle;

/// SETP/SETM/SETE [X0]!, X1!, X2
const SET: [u32; 3] = [0x19C2_0420, 0x19C2_4420, 0x19C2_8420];
/// SETGP/SETGM/SETGE [X0]!, X1!, X2
const SETG: [u32; 3] = [0x1DC2_0420, 0x1DC2_4420, 0x1DC2_8420];
/// CPYP/CPYM/CPYE [X0]!, [X1]!, X2!
const CPY: [u32; 3] = [0x1D01_0440, 0x1D41_0440, 0x1D81_0440];
/// CPYFP/CPYFM/CPYFE [X0]!, [X1]!, X2!
const CPYF: [u32; 3] = [0x1901_0440, 0x1941_0440, 0x1981_0440];

const GUARD: u64 = 0x2ff0;
const DST: u64 = 0x3000;

fn run(cpu: &mut Oracle, ops: [u32; 3]) {
    cpu.set_pc(0x1000);
    for op in ops {
        cpu.step(op);
    }
    assert_eq!(cpu.get_pc(), 0x100C, "no exception (ESR_EL3={:#x})", cpu.esr_el3());
}

/// Fills [GUARD, GUARD+0x60) with 0xEE, runs the sequence, and returns that window.
fn set(ops: [u32; 3], addr: u64, len: u64) -> (Oracle, Vec<u8>) {
    let mut cpu = Oracle::new();
    cpu.write_mem(GUARD, &[0xEE; 0x60]);
    cpu.set_x(0, addr);
    cpu.set_x(1, len);
    cpu.set_x(2, 0x41);
    run(&mut cpu, ops);
    let mut buf = vec![0u8; 0x60];
    cpu.read_mem(GUARD, &mut buf);
    (cpu, buf)
}

fn expect_fill(buf: &[u8], addr: u64, len: u64) {
    let (lo, hi) = ((addr - GUARD) as usize, (addr - GUARD + len) as usize);
    assert!(buf[..lo].iter().all(|&b| b == 0xEE), "bytes before the range changed");
    assert!(buf[lo..hi].iter().all(|&b| b == 0x41), "range not filled: {buf:02x?}");
    assert!(buf[hi..].iter().all(|&b| b == 0xEE), "bytes after the range changed");
}

#[test]
fn set_fills_range() {
    let (cpu, buf) = set(SET, DST, 16);
    expect_fill(&buf, DST, 16);
    assert_eq!(cpu.get_x(0), DST + 16);
    assert_eq!(cpu.get_x(1), 0);
}

#[test]
fn set_unaligned() {
    let (_, buf) = set(SET, DST + 5, 6);
    expect_fill(&buf, DST + 5, 6);
}

#[test]
fn set_zero_length_writes_nothing() {
    let (cpu, buf) = set(SET, DST, 0);
    assert!(buf.iter().all(|&b| b == 0xEE));
    assert_eq!(cpu.get_x(0), DST);
}

#[test]
fn setg_fills_range() {
    let (_, buf) = set(SETG, DST, 32);
    expect_fill(&buf, DST, 32);
}

fn pattern() -> [u8; 16] {
    core::array::from_fn(|i| 0x10 + i as u8)
}

/// Copies 16 bytes `src` -> `dst` and returns [GUARD, GUARD+0x40).
fn copy(ops: [u32; 3], dst: u64, src: u64) -> Vec<u8> {
    let mut cpu = Oracle::new();
    cpu.write_mem(GUARD, &[0xEE; 0x40]);
    cpu.write_mem(src, &pattern());
    cpu.set_x(0, dst);
    cpu.set_x(1, src);
    cpu.set_x(2, 16);
    run(&mut cpu, ops);
    assert_eq!(cpu.get_x(2), 0);
    let mut buf = vec![0u8; 0x40];
    cpu.read_mem(GUARD, &mut buf);
    buf
}

#[test]
fn cpy_copies() {
    let buf = copy(CPY, DST, 0x5000);
    assert_eq!(buf[..0x10], [0xEE; 0x10]);
    assert_eq!(buf[0x10..0x20], pattern());
    assert_eq!(buf[0x20..], [0xEE; 0x20]);
}

#[test]
fn cpy_overlapping_is_memmove() {
    let buf = copy(CPY, DST + 3, DST);
    assert_eq!(buf[0x10..0x13], [0x10, 0x11, 0x12]);
    assert_eq!(buf[0x13..0x23], pattern());
    assert_eq!(buf[0x23..], [0xEE; 0x1d]);
}

#[test]
fn cpyf_copies() {
    let buf = copy(CPYF, DST, 0x5000);
    assert_eq!(buf[..0x10], [0xEE; 0x10]);
    assert_eq!(buf[0x10..0x20], pattern());
    assert_eq!(buf[0x20..], [0xEE; 0x20]);
}
