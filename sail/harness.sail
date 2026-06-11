/* =============================================================================
 * harness.sail
 *
 * Stable, C-callable wrapper functions for the differential-test oracle.
 *
 * Compile this file TOGETHER WITH the arm-v9.4-a model, appended LAST:
 *
 *     sail -c -c_no_main <SAIL_SRCS from arm-v9.4-a/Makefile> sail/harness.sail -o model
 *
 * (Do NOT include src/elfmain.sail — it defines `main` and the emulator loop;
 * this file replaces it as the entry-point provider.)
 *
 * Because Sail mangles top-level names predictably (`z` prefix, and any `z`
 * INSIDE a name escapes to `zz`), these become:  zinit_harness, zset_gpr,
 * zget_gpr, zset_pc, zget_pc, zset_nzzcv, zget_nzzcv, zstep_a64 — which
 * csrc/shim.c calls.
 *
 * NOTE (Sail >= 0.18 C backend): unreachable code is dead-code-eliminated, so
 * every wrapper here must be kept alive with `--c-preserve <name>` (Sail-level
 * name, un-mangled) on the sail command line — see README Step 2.
 *
 * Deliberately uses ONLY fixed-width bitvector types (<= 64 bits) at the ABI so
 * the generated C signatures are plain `uint64_t`/`unit` with NO GMP `sail_int`
 * in the interface. SVE/FP (wide) registers are intentionally out of scope for
 * this first milestone (one integer instruction end-to-end); see README.
 *
 * Accessor names below were VERIFIED against rems-project/sail-arm arm-v9.4-a:
 *   X_set(n, width, value) / X_read(n, width)   src/v8_base.sail
 *   register _PC : bits(64)                      src/v8_base.sail
 *   register PSTATE : ProcState  (N/Z/C/V : bits(1))
 *   __InitSystem()                               src/reset.sail
 *   __DecodeExecute / __SetThisInstrDetails / __DefaultCond / SEE /
 *   __BranchTaken / __ExclusiveMonitorSet        src/fetch.sail, decode_start.sail
 * ===========================================================================*/

/* No $include needed: this file is appended after the full model, which already
 * defines everything we reference. */

/* ----- bring the model into a usable AArch64 state --------------------------
 * __InitSystem() = DBGEN low + TakeReset(true) + enable the system counter —
 * exactly what the model's own emulator main() runs first. After it the core
 * is at the highest EL in AArch64 with a sane PSTATE.                          */
function init_harness() -> unit = {
    /* The model's impdef default for the maximum STREAMING vector length is
     * 512 bits (a realistic hardware choice); raise it to MAX_VL so SVL ==
     * VL == 2048 and all 256 ZA rows are architecturally live.               */
    __max_implemented_smeveclen = 2048;
    __InitSystem();
    /* Enable SVE + FP at EL3 (we run at the highest EL after reset):
     * CheckSVEEnabled() traps when CPTR_EL3.EZ == 0 or CPTR_EL3.TFP == 1.
     * ZCR_EL3.LEN = 0b1111 requests the maximum vector length, clamped by
     * the model to MaxImplementedVL() = MAX_VL = 2048 bits.                  */
    CPTR_EL3[EZ] = 0b1;
    CPTR_EL3[TFP] = 0b0;
    ZCR_EL3[LEN] = 0b1111;
    /* ...and SME: CheckSMEEnabled() traps at EL3 when CPTR_EL3.ESM == 0.
     * SMCR_EL3.LEN = max streaming vector length (SVL), like ZCR above.      */
    CPTR_EL3[ESM] = 0b1;
    SMCR_EL3[LEN] = 0b1111;
    ()
}

/* ----- general purpose registers (X0..X30) ---------------------------------
 * Model accessor is X_set(n, width, value) / X_read(n, width); n==31 reads as
 * zero and writes are dropped (XZR behaviour). Indices come in as bits(8) to
 * keep the ABI GMP-free.                                                       */
function set_gpr(n: bits(8), value: bits(64)) -> unit = {
    X_set(UInt(n), 64, value)
}

function get_gpr(n: bits(8)) -> bits(64) = {
    X_read(UInt(n), 64)
}

/* ----- SVE Z/P registers (chunked 64-bit access) ----------------------------
 * Operates directly on the ARCHITECTURAL storage (_Z : vector(32, bits(2048)),
 * _P : vector(16, bits(256))), independent of the runtime VL — full-width
 * access for the oracle regardless of ZCR.LEN, and no GMP in the ABI.
 * Z chunk index 0..31 (2048/64), P chunk index 0..3 (256/64); Elem_read/
 * Elem_set are the model's own element helpers (builtins.sail).              */
function set_z_chunk(n: bits(8), idx: bits(8), value: bits(64)) -> unit = {
    let r = UInt(n[4 .. 0]);
    _Z[r] = Elem_set(_Z[r], UInt(idx[4 .. 0]), 64, value)
}

function get_z_chunk(n: bits(8), idx: bits(8)) -> bits(64) = {
    Elem_read(_Z[UInt(n[4 .. 0])], UInt(idx[4 .. 0]), 64)
}

function set_p_chunk(n: bits(8), idx: bits(8), value: bits(64)) -> unit = {
    let r = UInt(n[3 .. 0]);
    _P[r] = Elem_set(_P[r], UInt(idx[1 .. 0]), 64, value)
}

function get_p_chunk(n: bits(8), idx: bits(8)) -> bits(64) = {
    Elem_read(_P[UInt(n[3 .. 0])], UInt(idx[1 .. 0]), 64)
}

/* Current effective vector length in bits (after ZCR clamping), so the host
 * knows how many chunks are architecturally live.                            */
function get_vl() -> bits(64) = {
    integer_subrange(CurrentVL_read(), 63, 0)
}

/* ----- FP control/status registers ------------------------------------------
 * FPSR carries the cumulative FP exception flags (IOC/DZC/OFC/UFC/IXC/QC...)
 * — the main diffing target for FP instructions. FPCR holds rounding mode
 * etc. Both are 64-bit bitfield registers; Mk_*_Type is the model's own
 * whole-register constructor.                                                */
function set_fpcr(value: bits(64)) -> unit = {
    FPCR = Mk_FPCR_Type(value)
}

function get_fpcr() -> bits(64) = FPCR.bits

function set_fpsr(value: bits(64)) -> unit = {
    FPSR = Mk_FPSR_Type(value)
}

function get_fpsr() -> bits(64) = FPSR.bits

/* ----- SME ZA storage (chunked 64-bit access) --------------------------------
 * _ZA : vector(256, bits(2048)) — 256 rows of MAX_VL bits, same chunk scheme
 * as Z. Raw architectural storage access (valid regardless of PSTATE.ZA).    */
function set_za_chunk(row: bits(8), idx: bits(8), value: bits(64)) -> unit = {
    let r = UInt(row);
    _ZA[r] = Elem_set(_ZA[r], UInt(idx[4 .. 0]), 64, value)
}

function get_za_chunk(row: bits(8), idx: bits(8)) -> bits(64) = {
    Elem_read(_ZA[UInt(row)], UInt(idx[4 .. 0]), 64)
}

/* ----- PSTATE.SM/ZA observability (SVCR) -------------------------------------
 * bit1 = SM (streaming mode), bit0 = ZA (ZA storage enabled). Toggle them
 * architecturally by stepping SMSTART/SMSTOP (MSR SVCRSMZA, #imm) so the
 * model's side effects (Z/P/ZA zeroing, VL switch) happen correctly.         */
function get_svcr() -> bits(2) = PSTATE.SM @ PSTATE.ZA

/* ----- stack pointer ---------------------------------------------------------
 * SP_read/SP_set are the model's banked-SP accessors: they follow
 * PSTATE.SP (SPSel) and PSTATE.EL, i.e. SP_EL3 in the harness reset state.   */
function set_sp(value: bits(64)) -> unit = SP_set(value)

function get_sp() -> bits(64) = SP_read()

/* ----- remaining PSTATE bits, packed ----------------------------------------
 * Layout (LSB first):
 *   [0]=F [1]=I [2]=A [3]=D  [5:4]=BTYPE  [6]=SSBS [7]=PAN [8]=UAO
 *   [9]=DIT [10]=TCO  [12:11]=EL  [13]=SP(SPSel)  [14]=ZA [15]=SM
 * The setter writes the maskable/user-visible bits only; EL, SM and ZA are
 * read-only here — change them architecturally (ERET, SMSTART/SMSTOP).       */
function get_pstate_bits() -> bits(64) = {
    ZeroExtend(PSTATE.SM @ PSTATE.ZA @ PSTATE.SP @ PSTATE.EL
               @ PSTATE.TCO @ PSTATE.DIT @ PSTATE.UAO @ PSTATE.PAN
               @ PSTATE.SSBS @ PSTATE.BTYPE
               @ PSTATE.D @ PSTATE.A @ PSTATE.I @ PSTATE.F, 64)
}

function set_pstate_bits(value: bits(64)) -> unit = {
    PSTATE.F = [value[0]];
    PSTATE.I = [value[1]];
    PSTATE.A = [value[2]];
    PSTATE.D = [value[3]];
    PSTATE.BTYPE = value[5 .. 4];
    PSTATE.SSBS = [value[6]];
    PSTATE.PAN = [value[7]];
    PSTATE.UAO = [value[8]];
    PSTATE.DIT = [value[9]];
    PSTATE.TCO = [value[10]];
    PSTATE.SP = [value[13]];
    ()
}

/* ----- FFR (SVE first-fault register, 256 bits = 4 chunks) ------------------ */
function set_ffr_chunk(idx: bits(8), value: bits(64)) -> unit = {
    _FFR = Elem_set(_FFR, UInt(idx[1 .. 0]), 64, value)
}

function get_ffr_chunk(idx: bits(8)) -> bits(64) = {
    Elem_read(_FFR, UInt(idx[1 .. 0]), 64)
}

/* ----- ZT0 (SME2 lookup table, 512 bits = 8 chunks) -------------------------- */
function set_zt0_chunk(idx: bits(8), value: bits(64)) -> unit = {
    _ZT0 = Elem_set(_ZT0, UInt(idx[2 .. 0]), 64, value)
}

function get_zt0_chunk(idx: bits(8)) -> bits(64) = {
    Elem_read(_ZT0, UInt(idx[2 .. 0]), 64)
}

/* ----- thread pointers (MRS/MSR targets; plain bits(64) registers) ----------- */
function set_tpidr_el0(value: bits(64)) -> unit = {
    TPIDR_EL0 = value
}

function get_tpidr_el0() -> bits(64) = TPIDR_EL0

function set_tpidrro_el0(value: bits(64)) -> unit = {
    TPIDRRO_EL0 = value
}

function get_tpidrro_el0() -> bits(64) = TPIDRRO_EL0

/* ----- exception observability (read-only) -----------------------------------
 * After a step that UNDEFs/faults, the model vectors to EL3: PC jumps to
 * VBAR_EL3+offset, ESR_EL3 holds the syndrome, ELR_EL3 the faulting PC,
 * FAR_EL3 the faulting address. Expose them so a diff harness can classify
 * trapped instructions instead of just seeing a wild PC.                     */
function get_esr_el3() -> bits(64) = ESR_EL3.bits  /* bitfield register */

function get_elr_el3() -> bits(64) = ELR_EL3       /* plain bits(64)    */

function get_far_el3() -> bits(64) = FAR_EL3

/* ----- program counter ----------------------------------------------------- */
function set_pc(value: bits(64)) -> unit = {
    _PC = value
}

function get_pc() -> bits(64) = {
    _PC
}

/* ----- condition flags NZCV (bits [3:0] = N,Z,C,V) ------------------------- */
function set_nzcv(flags: bits(4)) -> unit = {
    PSTATE.N = [flags[3]];
    PSTATE.Z = [flags[2]];
    PSTATE.C = [flags[1]];
    PSTATE.V = [flags[0]];
    ()
}

function get_nzcv() -> bits(4) = {
    PSTATE.N @ PSTATE.Z @ PSTATE.C @ PSTATE.V
}

/* ----- execute exactly one A64 instruction --------------------------------
 * Mirrors src/fetch.sail __InstructionExecute(), minus the instruction fetch
 * (the opcode is injected directly) and minus the debug/PMU/SS bookkeeping.
 * __DecodeExecute handles the SEE-retry decode protocol internally; the outer
 * catch mirrors the model's own handling of UNDEFINED/exception-taken.         */
function step_a64(opcode: bits(32)) -> unit = {
    SEE = -1;
    __BranchTaken = false;
    __ExclusiveMonitorSet = false;
    __SetThisInstrDetails(__A64, opcode, __DefaultCond(__A64));
    try {
        __DecodeExecute(__A64, opcode)
    } catch {
        exn if (IsSEE(exn) | IsUNDEFINED(exn)) | IsUNPREDICTABLE(exn) => {
            /* AArch64_UndefinedFault() takes the exception to EL3 and then
             * throws Error_ExceptionTaken (via EndOfInstruction). A handler's
             * own throw is NOT caught by this try's other arms — upstream the
             * model relies on __TopLevel for that; we are the top level, so
             * catch it here. */
            try {
                AArch64_UndefinedFault()
            } catch {
                exn if IsExceptionTaken(exn) => ()
            }
        },
        exn if IsExceptionTaken(exn) => ()
    };
    if not_bool(__BranchTaken) then {
        _PC = (_PC + 4)[63 .. 0]
    };
    ()
}

/* ----- main stub ------------------------------------------------------------
 * Even with --c-no-main, the Sail C backend unconditionally emits a
 * `model_main()` that calls `zmain` — so a Sail `main` must exist or the
 * generated C does not compile. (We exclude src/elfmain.sail, which normally
 * provides it along with the emulator fetch loop we don't want.) Never called
 * through the oracle ABI.                                                      */
function main() -> unit = ()
