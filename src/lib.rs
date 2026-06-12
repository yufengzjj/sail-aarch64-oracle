//! Safe Rust wrapper around the Sail arm-v9.4-a model (via `csrc/shim.cpp`).
//!
//! # Concurrency model
//!
//! The model is generated with Sail's C++ backend (`sail --cpp`): all
//! architectural state lives in members of a generated C++ class, so **each
//! [`Oracle`] is an independent CPU** — construct as many per process as you
//! like, they do not affect each other. **Memory is isolated too**: every
//! instance owns its own Sail memory-block lists, which the shim swaps into
//! the runtime globals around each model call.
//!
//! The Sail C *runtime* underneath is NOT thread-safe (process-global GMP
//! scratch temporaries plus the memory-context swap). Every call into the
//! model therefore takes a process-wide mutex, which makes [`Oracle`]
//! **`Send + Sync`**: instances can be moved to / shared between threads
//! freely and misuse cannot crash — concurrent calls are simply serialized.
//! Note this means threads give you safety, not speedup; for real
//! parallelism keep using process-per-test (`cargo nextest run`).

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

/// Architectural (maximum) SVE vector width handled by the model: 2048 bits.
pub const Z_BITS: usize = 2048;
/// Architectural predicate width: 256 bits.
pub const P_BITS: usize = 256;
/// 64-bit chunks per Z register.
pub const Z_CHUNKS: usize = Z_BITS / 64;
/// 64-bit chunks per P register.
pub const P_CHUNKS: usize = P_BITS / 64;
/// SME ZA storage rows (each row is [`Z_BITS`] wide).
pub const ZA_ROWS: usize = 256;
/// ZT0 (SME2 lookup table) width: 512 bits = 8 chunks.
pub const ZT0_CHUNKS: usize = 8;

/// Serializes every call into the Sail model, across ALL instances: the
/// generated per-instance state is fine, but the Sail C runtime keeps
/// process-global scratch (GMP temporaries; the per-instance memory lists
/// are swapped through globals) that is touched during every call.
static MODEL_LOCK: Mutex<()> = Mutex::new(());

fn model_lock() -> MutexGuard<'static, ()> {
    // The guarded sections only run C/C++ code, which does not panic, so a
    // poisoned lock cannot indicate torn model state — keep going.
    MODEL_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An independent instance of the Sail AArch64 model.
///
/// `Send + Sync`: every model call is serialized by a process-wide mutex
/// (see the module docs), so cross-thread use is safe — just not parallel.
pub struct Oracle {
    h: *mut c_void,
}

// SAFETY: the handle's instance state is only reachable through this struct
// (aliasing governed by &/&mut as usual), and every FFI call — including the
// process-global runtime scratch and memory-context swap it touches — runs
// under MODEL_LOCK.
unsafe impl Send for Oracle {}
unsafe impl Sync for Oracle {}

impl Oracle {
    /// Create and reset a fresh, independent model instance.
    pub fn new() -> Self {
        let _g = model_lock();
        // SAFETY: oracle_new fully initializes the instance (runtime setup is
        // refcounted process-wide; registers + reset run per instance).
        let h = unsafe { oracle_new() };
        assert!(!h.is_null(), "oracle_new returned null");
        Oracle { h }
    }

    /// Write Xn (n in 0..=30). n==31 is the zero register — writes are dropped.
    pub fn set_x(&mut self, n: u32, value: u64) {
        debug_assert!(n <= 31);
        let _g = model_lock();
        unsafe { oracle_set_gpr(self.h, n, value) }
    }

    /// Read Xn (n in 0..=30). n==31 reads as zero.
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

    /// Set condition flags from the low 4 bits: bit3=N, bit2=Z, bit1=C, bit0=V.
    pub fn set_nzcv(&mut self, flags: u32) {
        let _g = model_lock();
        unsafe { oracle_set_nzcv(self.h, flags & 0xf) }
    }

    /// Read condition flags (same bit layout as [`Oracle::set_nzcv`]).
    pub fn get_nzcv(&self) -> u32 {
        let _g = model_lock();
        unsafe { oracle_get_nzcv(self.h) & 0xf }
    }

    /// Decode and execute a single 32-bit A64 instruction.
    pub fn step(&mut self, opcode: u32) {
        let _g = model_lock();
        unsafe { oracle_step(self.h, opcode) }
    }

    /// Write SVE Zn (n in 0..=31) from little-endian 64-bit chunks: `data[0]`
    /// holds bits 63:0. Chunks beyond `data.len()` (up to [`Z_CHUNKS`]) are
    /// zeroed. Writes the full 2048-bit architectural register.
    pub fn set_z(&mut self, n: u32, data: &[u64]) {
        debug_assert!(n <= 31 && data.len() <= Z_CHUNKS);
        let _g = model_lock();
        for i in 0..Z_CHUNKS {
            let v = data.get(i).copied().unwrap_or(0);
            unsafe { oracle_set_z(self.h, n, i as u32, v) }
        }
    }

    /// Read the full 2048-bit architectural SVE Zn as 64-bit chunks.
    pub fn get_z(&self, n: u32) -> [u64; Z_CHUNKS] {
        debug_assert!(n <= 31);
        let _g = model_lock();
        let mut out = [0u64; Z_CHUNKS];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = unsafe { oracle_get_z(self.h, n, i as u32) };
        }
        out
    }

    /// Write SVE predicate Pn (n in 0..=15); same chunk layout as [`Oracle::set_z`].
    pub fn set_p(&mut self, n: u32, data: &[u64]) {
        debug_assert!(n <= 15 && data.len() <= P_CHUNKS);
        let _g = model_lock();
        for i in 0..P_CHUNKS {
            let v = data.get(i).copied().unwrap_or(0);
            unsafe { oracle_set_p(self.h, n, i as u32, v) }
        }
    }

    /// Read the full 256-bit architectural predicate Pn as 64-bit chunks.
    pub fn get_p(&self, n: u32) -> [u64; P_CHUNKS] {
        debug_assert!(n <= 15);
        let _g = model_lock();
        let mut out = [0u64; P_CHUNKS];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = unsafe { oracle_get_p(self.h, n, i as u32) };
        }
        out
    }

    /// Current effective SVE vector length in bits (after ZCR clamping);
    /// `init_harness` requests the maximum, so this is 2048 for this model.
    pub fn vl_bits(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_vl(self.h) }
    }

    /// FP control register (rounding mode etc.).
    pub fn set_fpcr(&mut self, value: u64) {
        let _g = model_lock();
        unsafe { oracle_set_fpcr(self.h, value) }
    }

    pub fn get_fpcr(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_fpcr(self.h) }
    }

    /// FP status register — cumulative exception flags (IOC/DZC/OFC/UFC/IXC,
    /// QC). This is where FP implementations actually diverge; clear it
    /// before an op and diff it after.
    pub fn set_fpsr(&mut self, value: u64) {
        let _g = model_lock();
        unsafe { oracle_set_fpsr(self.h, value) }
    }

    pub fn get_fpsr(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_fpsr(self.h) }
    }

    /// Write a row (0..=255) of SME ZA storage; chunk layout as [`Oracle::set_z`].
    pub fn set_za_row(&mut self, row: u32, data: &[u64]) {
        debug_assert!((row as usize) < ZA_ROWS && data.len() <= Z_CHUNKS);
        let _g = model_lock();
        for i in 0..Z_CHUNKS {
            let v = data.get(i).copied().unwrap_or(0);
            unsafe { oracle_set_za(self.h, row, i as u32, v) }
        }
    }

    /// Read a full 2048-bit ZA row.
    pub fn get_za_row(&self, row: u32) -> [u64; Z_CHUNKS] {
        debug_assert!((row as usize) < ZA_ROWS);
        let _g = model_lock();
        let mut out = [0u64; Z_CHUNKS];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = unsafe { oracle_get_za(self.h, row, i as u32) };
        }
        out
    }

    /// SVCR view of PSTATE: bit1 = SM (streaming mode), bit0 = ZA enabled.
    /// Toggle architecturally by stepping SMSTART (`0xD503477F`) / SMSTOP
    /// (`0xD503467F`) so mode-switch side effects apply.
    pub fn svcr(&self) -> u32 {
        let _g = model_lock();
        unsafe { oracle_get_svcr(self.h) }
    }

    /// Current (banked) stack pointer — follows PSTATE.SP/EL, i.e. SP_EL3 in
    /// the reset state.
    pub fn set_sp(&mut self, value: u64) {
        let _g = model_lock();
        unsafe { oracle_set_sp(self.h, value) }
    }

    pub fn get_sp(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_sp(self.h) }
    }

    /// Packed PSTATE word. Layout (LSB first): `[0]`=F `[1]`=I `[2]`=A `[3]`=D,
    /// `[5:4]`=BTYPE, `[6]`=SSBS `[7]`=PAN `[8]`=UAO `[9]`=DIT `[10]`=TCO,
    /// `[12:11]`=EL, `[13]`=SP(SPSel), `[14]`=ZA `[15]`=SM.
    pub fn get_pstate(&self) -> u64 {
        let _g = model_lock();
        unsafe { oracle_get_pstate(self.h) }
    }

    /// Write the maskable PSTATE bits (DAIF/BTYPE/SSBS/PAN/UAO/DIT/TCO/SPSel).
    /// EL, SM and ZA in the word are ignored — change those architecturally
    /// (ERET, SMSTART/SMSTOP).
    pub fn set_pstate(&mut self, value: u64) {
        let _g = model_lock();
        unsafe { oracle_set_pstate(self.h, value) }
    }

    /// SVE first-fault register (256 bits), chunk layout as [`Oracle::set_p`].
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

    /// SME2 ZT0 lookup table (512 bits = [`ZT0_CHUNKS`] chunks).
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

    /// EL0 thread pointers (common MRS/MSR targets).
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

    /// Exception syndrome/link/fault-address at EL3 (read-only): after a
    /// step that traps, ESR_EL3 classifies it, ELR_EL3 holds the faulting
    /// PC, FAR_EL3 the faulting address; PC will have vectored to
    /// VBAR_EL3 + offset.
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

    // ---- direct RAM access (byte-addressed, little-endian) -----------------
    // Reads/writes this instance's isolated memory. Unmapped addresses read as
    // zero; writing to a fresh page allocates it. This observes exactly what
    // the model's loads/stores see, so you can seed memory before `step` or
    // inspect it afterwards without issuing LDR/STR instructions.

    /// Read a single byte at `addr` (0 if the page was never written).
    pub fn read_mem_byte(&self, addr: u64) -> u8 {
        let _g = model_lock();
        unsafe { oracle_read_mem(self.h, addr) }
    }

    /// Write a single byte at `addr`.
    pub fn write_mem_byte(&mut self, addr: u64, byte: u8) {
        let _g = model_lock();
        unsafe { oracle_write_mem(self.h, addr, byte) }
    }

    /// Fill `buf` with the bytes at `addr..addr + buf.len()`.
    pub fn read_mem(&self, addr: u64, buf: &mut [u8]) {
        let _g = model_lock();
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = unsafe { oracle_read_mem(self.h, addr.wrapping_add(i as u64)) };
        }
    }

    /// Write `data` to memory starting at `addr`.
    pub fn write_mem(&mut self, addr: u64, data: &[u8]) {
        let _g = model_lock();
        for (i, &byte) in data.iter().enumerate() {
            unsafe { oracle_write_mem(self.h, addr.wrapping_add(i as u64), byte) }
        }
    }

    /// Read a little-endian `u64` at `addr` (matches AArch64 byte order).
    pub fn read_mem_u64(&self, addr: u64) -> u64 {
        let mut b = [0u8; 8];
        self.read_mem(addr, &mut b);
        u64::from_le_bytes(b)
    }

    /// Write `value` as a little-endian `u64` at `addr`.
    pub fn write_mem_u64(&mut self, addr: u64, value: u64) {
        self.write_mem(addr, &value.to_le_bytes());
    }

    /// True iff the region containing `addr` has ever been written (a backing
    /// block exists). Because [`read_mem_byte`](Self::read_mem_byte) returns 0
    /// for untouched memory, this is the only way to distinguish "explicitly
    /// wrote 0" from "never touched". Granularity is the runtime's block size
    /// (currently 16 MiB), so a single write maps its whole containing block.
    pub fn is_mapped(&self, addr: u64) -> bool {
        let _g = model_lock();
        unsafe { oracle_is_mapped(self.h, addr) }
    }

    /// Convenience: snapshot of X0..X30, PC and NZCV for diffing against your model.
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

/// Minimal comparable architectural state (extend with Z/P/FP as you wire them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuState {
    pub x: [u64; 31],
    pub pc: u64,
    pub nzcv: u32,
}
