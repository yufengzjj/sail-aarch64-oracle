//! Safe Rust wrapper around the Sail arm-v9.4-a model (via `csrc/shim.cpp`).
//!
//! Each [`Oracle`] is an independent CPU (state + memory). The underlying Sail
//! C runtime is NOT thread-safe, so every model call takes a process-wide
//! mutex: [`Oracle`] is `Send + Sync`, but threads give safety, not speedup
//! (calls serialize) — for parallelism use process-per-test (`cargo nextest`).

use std::ffi::c_void;
use std::sync::{Mutex, MutexGuard, PoisonError};

extern "C" {
    fn oracle_new() -> *mut c_void;
    fn oracle_free(h: *mut c_void);
    fn oracle_set_gpr(h: *mut c_void, n: u32, value: u64);
    fn oracle_get_gpr(h: *mut c_void, n: u32) -> u64;
    fn oracle_set_pc(h: *mut c_void, value: u64);
    fn oracle_get_pc(h: *mut c_void) -> u64;
    fn oracle_set_nzcv(h: *mut c_void, flags: u32);
    fn oracle_get_nzcv(h: *mut c_void) -> u32;
    fn oracle_step(h: *mut c_void, opcode: u32);
    fn oracle_set_z(h: *mut c_void, n: u32, chunk: u32, value: u64);
    fn oracle_get_z(h: *mut c_void, n: u32, chunk: u32) -> u64;
    fn oracle_set_p(h: *mut c_void, n: u32, chunk: u32, value: u64);
    fn oracle_get_p(h: *mut c_void, n: u32, chunk: u32) -> u64;
    fn oracle_get_vl(h: *mut c_void) -> u64;
    fn oracle_set_fpcr(h: *mut c_void, value: u64);
    fn oracle_get_fpcr(h: *mut c_void) -> u64;
    fn oracle_set_fpsr(h: *mut c_void, value: u64);
    fn oracle_get_fpsr(h: *mut c_void) -> u64;
    fn oracle_set_za(h: *mut c_void, row: u32, chunk: u32, value: u64);
    fn oracle_get_za(h: *mut c_void, row: u32, chunk: u32) -> u64;
    fn oracle_get_svcr(h: *mut c_void) -> u32;
    fn oracle_set_sp(h: *mut c_void, value: u64);
    fn oracle_get_sp(h: *mut c_void) -> u64;
    fn oracle_get_pstate(h: *mut c_void) -> u64;
    fn oracle_set_pstate(h: *mut c_void, value: u64);
    fn oracle_set_ffr(h: *mut c_void, chunk: u32, value: u64);
    fn oracle_get_ffr(h: *mut c_void, chunk: u32) -> u64;
    fn oracle_set_zt0(h: *mut c_void, chunk: u32, value: u64);
    fn oracle_get_zt0(h: *mut c_void, chunk: u32) -> u64;
    fn oracle_set_tpidr_el0(h: *mut c_void, value: u64);
    fn oracle_get_tpidr_el0(h: *mut c_void) -> u64;
    fn oracle_set_tpidrro_el0(h: *mut c_void, value: u64);
    fn oracle_get_tpidrro_el0(h: *mut c_void) -> u64;
    fn oracle_get_esr_el3(h: *mut c_void) -> u64;
    fn oracle_get_elr_el3(h: *mut c_void) -> u64;
    fn oracle_get_far_el3(h: *mut c_void) -> u64;
    fn oracle_read_mem(h: *mut c_void, addr: u64) -> u8;
    fn oracle_write_mem(h: *mut c_void, addr: u64, byte: u8);
    fn oracle_is_mapped(h: *mut c_void, addr: u64) -> bool;
}

pub const Z_BITS: usize = 2048;
pub const P_BITS: usize = 256;
pub const Z_CHUNKS: usize = Z_BITS / 64;
pub const P_CHUNKS: usize = P_BITS / 64;
pub const ZA_ROWS: usize = 256;
pub const ZT0_CHUNKS: usize = 8;

// Serializes every model call across ALL instances: per-instance state is
// fine, but the Sail C runtime keeps process-global scratch (GMP temporaries;
// memory lists swapped through globals) touched on every call.
static MODEL_LOCK: Mutex<()> = Mutex::new(());

fn model_lock() -> MutexGuard<'static, ()> {
    // Guarded sections run only C/C++ (no panics), so a poisoned lock cannot
    // mean torn model state — keep going.
    MODEL_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct Oracle {
    h: *mut c_void,
}

// SAFETY: instance state is reachable only through this struct (&/&mut
// aliasing) and every FFI call runs under MODEL_LOCK.
unsafe impl Send for Oracle {}
unsafe impl Sync for Oracle {}

impl Oracle {
    pub fn new() -> Self {
        let _g = model_lock();
        let h = unsafe { oracle_new() };
        assert!(!h.is_null(), "oracle_new returned null");
        Oracle { h }
    }

    /// n==31 is the zero register: writes dropped, reads as zero.
    pub fn set_x(&mut self, n: u32, value: u64) {
        debug_assert!(n <= 31);
        let _g = model_lock();
        unsafe { oracle_set_gpr(self.h, n, value) }
    }

    pub fn get_x(&self, n: u32) -> u64 {
        debug_assert!(n <= 31);
        let _g = model_lock();
        unsafe { oracle_get_gpr(self.h, n) }
    }

    pub fn set_pc(&mut self, value: u64) {
        let _g = model_lock();
        unsafe { oracle_set_pc(self.h, value) }
    }

    pub fn get_pc(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_pc(self.h) }
    }

    /// Low 4 bits: bit3=N, bit2=Z, bit1=C, bit0=V.
    pub fn set_nzcv(&mut self, flags: u32) {
        let _g = model_lock();
        unsafe { oracle_set_nzcv(self.h, flags & 0xf) }
    }

    pub fn get_nzcv(&self) -> u32 {
        let _g = model_lock();
        unsafe { oracle_get_nzcv(self.h) & 0xf }
    }

    pub fn step(&mut self, opcode: u32) {
        let _g = model_lock();
        unsafe { oracle_step(self.h, opcode) }
    }

    /// Little-endian chunks (`data[0]` = bits 63:0); missing chunks zeroed.
    /// Always writes the full architectural register, independent of VL.
    pub fn set_z(&mut self, n: u32, data: &[u64]) {
        debug_assert!(n <= 31 && data.len() <= Z_CHUNKS);
        let _g = model_lock();
        for i in 0..Z_CHUNKS {
            let v = data.get(i).copied().unwrap_or(0);
            unsafe { oracle_set_z(self.h, n, i as u32, v) }
        }
    }

    pub fn get_z(&self, n: u32) -> [u64; Z_CHUNKS] {
        debug_assert!(n <= 31);
        let _g = model_lock();
        let mut out = [0u64; Z_CHUNKS];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = unsafe { oracle_get_z(self.h, n, i as u32) };
        }
        out
    }

    pub fn set_p(&mut self, n: u32, data: &[u64]) {
        debug_assert!(n <= 15 && data.len() <= P_CHUNKS);
        let _g = model_lock();
        for i in 0..P_CHUNKS {
            let v = data.get(i).copied().unwrap_or(0);
            unsafe { oracle_set_p(self.h, n, i as u32, v) }
        }
    }

    pub fn get_p(&self, n: u32) -> [u64; P_CHUNKS] {
        debug_assert!(n <= 15);
        let _g = model_lock();
        let mut out = [0u64; P_CHUNKS];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = unsafe { oracle_get_p(self.h, n, i as u32) };
        }
        out
    }

    /// Effective VL in bits; 2048 at reset (`init_harness` requests the max).
    pub fn vl_bits(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_vl(self.h) }
    }

    /// Set non-streaming SVE VL by programming `ZCR_EL3.LEN`. `bits` is
    /// quantized to `(bits/128)-1` (clamped to 128..=2048) and the model rounds
    /// down to an implemented length; returns the effective VL. Does not touch
    /// the streaming VL (see [`set_svl`](Self::set_svl)).
    pub fn set_vl(&mut self, bits: u32) -> u64 {
        // MSR ZCR_EL3, X0 (S3_6_C1_C2_0). LEN in 0..=15 selects (LEN+1)*128 b.
        const MSR_ZCR_EL3_X0: u32 = 0xD51E_1200;
        let len = (bits.max(128) / 128 - 1).min(15) as u64;
        self.write_sysreg_len(MSR_ZCR_EL3_X0, len);
        self.vl_bits()
    }

    /// Set streaming VL (SVL) by programming `SMCR_EL3.LEN`; sizes ZA and is
    /// the VL in effect while `PSTATE.SM == 1`. The returned `vl_bits()` only
    /// reflects it when already in streaming mode. Changing SVL *while*
    /// streaming zeroes Z/P/FFR; staging it while not streaming does not.
    pub fn set_svl(&mut self, bits: u32) -> u64 {
        // MSR SMCR_EL3, X0 (S3_6_C1_C2_6).
        const MSR_SMCR_EL3_X0: u32 = 0xD51E_12C0;
        let len = (bits.max(128) / 128 - 1).min(15) as u64;
        self.write_sysreg_len(MSR_SMCR_EL3_X0, len);
        self.vl_bits()
    }

    /// Restores X0 and PC afterwards (the MSR would otherwise advance PC by 4).
    fn write_sysreg_len(&mut self, msr_opcode: u32, len: u64) {
        let saved_x0 = self.get_x(0);
        let saved_pc = self.get_pc();
        self.set_x(0, len);
        self.step(msr_opcode);
        self.set_x(0, saved_x0);
        self.set_pc(saved_pc);
    }

    pub fn set_fpcr(&mut self, value: u64) {
        let _g = model_lock();
        unsafe { oracle_set_fpcr(self.h, value) }
    }

    pub fn get_fpcr(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_fpcr(self.h) }
    }

    pub fn set_fpsr(&mut self, value: u64) {
        let _g = model_lock();
        unsafe { oracle_set_fpsr(self.h, value) }
    }

    pub fn get_fpsr(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_fpsr(self.h) }
    }

    pub fn set_za_row(&mut self, row: u32, data: &[u64]) {
        debug_assert!((row as usize) < ZA_ROWS && data.len() <= Z_CHUNKS);
        let _g = model_lock();
        for i in 0..Z_CHUNKS {
            let v = data.get(i).copied().unwrap_or(0);
            unsafe { oracle_set_za(self.h, row, i as u32, v) }
        }
    }

    pub fn get_za_row(&self, row: u32) -> [u64; Z_CHUNKS] {
        debug_assert!((row as usize) < ZA_ROWS);
        let _g = model_lock();
        let mut out = [0u64; Z_CHUNKS];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = unsafe { oracle_get_za(self.h, row, i as u32) };
        }
        out
    }

    /// bit1 = SM (streaming), bit0 = ZA enabled. Read-only: toggle by stepping
    /// SMSTART (`0xD503477F`) / SMSTOP (`0xD503467F`) so side effects apply.
    pub fn svcr(&self) -> u32 {
        let _g = model_lock();
        unsafe { oracle_get_svcr(self.h) }
    }

    /// Banked: follows PSTATE.SP/EL (SP_EL3 at reset).
    pub fn set_sp(&mut self, value: u64) {
        let _g = model_lock();
        unsafe { oracle_set_sp(self.h, value) }
    }

    pub fn get_sp(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_sp(self.h) }
    }

    /// Packed PSTATE (LSB first): `[0]`=F `[1]`=I `[2]`=A `[3]`=D, `[5:4]`=BTYPE,
    /// `[6]`=SSBS `[7]`=PAN `[8]`=UAO `[9]`=DIT `[10]`=TCO, `[12:11]`=EL,
    /// `[13]`=SP(SPSel), `[14]`=ZA `[15]`=SM.
    pub fn get_pstate(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_pstate(self.h) }
    }

    /// Only the maskable bits are written; EL/SM/ZA are ignored (change those
    /// architecturally via ERET, SMSTART/SMSTOP).
    pub fn set_pstate(&mut self, value: u64) {
        let _g = model_lock();
        unsafe { oracle_set_pstate(self.h, value) }
    }

    pub fn set_ffr(&mut self, data: &[u64]) {
        debug_assert!(data.len() <= P_CHUNKS);
        let _g = model_lock();
        for i in 0..P_CHUNKS {
            let v = data.get(i).copied().unwrap_or(0);
            unsafe { oracle_set_ffr(self.h, i as u32, v) }
        }
    }

    pub fn get_ffr(&self) -> [u64; P_CHUNKS] {
        let _g = model_lock();
        let mut out = [0u64; P_CHUNKS];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = unsafe { oracle_get_ffr(self.h, i as u32) };
        }
        out
    }

    pub fn set_zt0(&mut self, data: &[u64]) {
        debug_assert!(data.len() <= ZT0_CHUNKS);
        let _g = model_lock();
        for i in 0..ZT0_CHUNKS {
            let v = data.get(i).copied().unwrap_or(0);
            unsafe { oracle_set_zt0(self.h, i as u32, v) }
        }
    }

    pub fn get_zt0(&self) -> [u64; ZT0_CHUNKS] {
        let _g = model_lock();
        let mut out = [0u64; ZT0_CHUNKS];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = unsafe { oracle_get_zt0(self.h, i as u32) };
        }
        out
    }

    pub fn set_tpidr_el0(&mut self, value: u64) {
        let _g = model_lock();
        unsafe { oracle_set_tpidr_el0(self.h, value) }
    }

    pub fn get_tpidr_el0(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_tpidr_el0(self.h) }
    }

    pub fn set_tpidrro_el0(&mut self, value: u64) {
        let _g = model_lock();
        unsafe { oracle_set_tpidrro_el0(self.h, value) }
    }

    pub fn get_tpidrro_el0(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_tpidrro_el0(self.h) }
    }

    /// EL3 exception state, read-only. After a trapping step PC has vectored to
    /// VBAR_EL3+offset; ESR classifies, ELR holds the faulting PC, FAR the addr.
    pub fn esr_el3(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_esr_el3(self.h) }
    }

    pub fn elr_el3(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_elr_el3(self.h) }
    }

    pub fn far_el3(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_far_el3(self.h) }
    }

    // Direct byte-addressed, little-endian access to this instance's memory —
    // the same bytes the model's loads/stores see. Unmapped addresses read as
    // zero; writing a fresh page allocates it.

    /// 0 if the address was never written.
    pub fn read_mem_byte(&self, addr: u64) -> u8 {
        let _g = model_lock();
        unsafe { oracle_read_mem(self.h, addr) }
    }

    pub fn write_mem_byte(&mut self, addr: u64, byte: u8) {
        let _g = model_lock();
        unsafe { oracle_write_mem(self.h, addr, byte) }
    }

    pub fn read_mem(&self, addr: u64, buf: &mut [u8]) {
        let _g = model_lock();
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = unsafe { oracle_read_mem(self.h, addr.wrapping_add(i as u64)) };
        }
    }

    pub fn write_mem(&mut self, addr: u64, data: &[u8]) {
        let _g = model_lock();
        for (i, &byte) in data.iter().enumerate() {
            unsafe { oracle_write_mem(self.h, addr.wrapping_add(i as u64), byte) }
        }
    }

    pub fn read_mem_u64(&self, addr: u64) -> u64 {
        let mut b = [0u8; 8];
        self.read_mem(addr, &mut b);
        u64::from_le_bytes(b)
    }

    pub fn write_mem_u64(&mut self, addr: u64, value: u64) {
        self.write_mem(addr, &value.to_le_bytes());
    }

    /// Whether `addr`'s region was ever written — the only way to tell "wrote 0"
    /// from "never touched" (reads return 0 either way). Granularity is the
    /// runtime's 16 MiB block, so one write maps its whole containing block.
    pub fn is_mapped(&self, addr: u64) -> bool {
        let _g = model_lock();
        unsafe { oracle_is_mapped(self.h, addr) }
    }

    pub fn snapshot(&self) -> CpuState {
        let mut x = [0u64; 31];
        for (i, slot) in x.iter_mut().enumerate() {
            *slot = self.get_x(i as u32);
        }
        CpuState { x, pc: self.get_pc(), nzcv: self.get_nzcv() }
    }
}

impl Default for Oracle {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Oracle {
    fn drop(&mut self) {
        let _g = model_lock();
        unsafe { oracle_free(self.h) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuState {
    pub x: [u64; 31],
    pub pc: u64,
    pub nzcv: u32,
}
