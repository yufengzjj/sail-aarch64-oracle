/* =============================================================================
 * harness.sail — C-callable wrappers for the oracle, appended LAST to the
 * arm-v9.4-a model (see README Step 2 for the full `sail --cpp` command).
 *
 * Build gotchas:
 *   - Do NOT include src/elfmain.sail; this file replaces it as the `main`
 *     provider (a `main` must exist even with --c-no-main — see the stub below).
 *   - Sail's C++ backend dead-code-eliminates unreferenced code, so every
 *     wrapper needs `--c-preserve <unmangled-name>` on the command line.
 *   - The ABI uses only fixed-width bitvectors (<= 64 b) so the generated C is
 *     plain uint64_t/unit with no GMP sail_int.
 *   - Name mangling: `z` prefix, and a literal `z` inside escapes to `zz`
 *     (set_nzcv -> zset_nzzcv) — shim.cpp calls the mangled names.
 * ===========================================================================*/

/* __InitSystem() resets to the highest EL (EL3) in AArch64, like the model's
 * own emulator main(). The rest enables SVE/FP/SME at EL3 — without these,
 * CheckSVE/SMEEnabled() trap (EZ/TFP/ESM gates) — and requests max VL/SVL.
 * __max_implemented_smeveclen defaults to 512; raise it to 2048 so SVL == VL
 * == 2048 and all 256 ZA rows are live. */
function init_harness() -> unit = {
    __max_implemented_smeveclen = 2048;
    __InitSystem();
    CPTR_EL3[EZ] = 0b1;
    CPTR_EL3[TFP] = 0b0;
    ZCR_EL3[LEN] = 0b1111;
    CPTR_EL3[ESM] = 0b1;
    SMCR_EL3[LEN] = 0b1111;
    ()
}

/* n==31 is XZR: reads zero, writes dropped. */
function set_gpr(n: bits(8), value: bits(64)) -> unit = {
    X_set(UInt(n), 64, value)
}

function get_gpr(n: bits(8)) -> bits(64) = {
    X_read(UInt(n), 64)
}

/* Direct access to architectural storage (_Z/_P), full-width regardless of the
 * runtime VL (ZCR.LEN). Z chunk 0..31, P chunk 0..3. */
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

function get_vl() -> bits(64) = {
    integer_subrange(CurrentVL_read(), 63, 0)
}

function set_fpcr(value: bits(64)) -> unit = {
    FPCR = Mk_FPCR_Type(value)
}

function get_fpcr() -> bits(64) = FPCR.bits

function set_fpsr(value: bits(64)) -> unit = {
    FPSR = Mk_FPSR_Type(value)
}

function get_fpsr() -> bits(64) = FPSR.bits

/* Raw architectural ZA access, valid regardless of PSTATE.ZA. */
function set_za_chunk(row: bits(8), idx: bits(8), value: bits(64)) -> unit = {
    let r = UInt(row);
    _ZA[r] = Elem_set(_ZA[r], UInt(idx[4 .. 0]), 64, value)
}

function get_za_chunk(row: bits(8), idx: bits(8)) -> bits(64) = {
    Elem_read(_ZA[UInt(row)], UInt(idx[4 .. 0]), 64)
}

/* bit1 = SM, bit0 = ZA. Read-only: toggle via SMSTART/SMSTOP so the model's
 * side effects (Z/P/ZA zeroing, VL switch) happen. */
function get_svcr() -> bits(2) = PSTATE.SM @ PSTATE.ZA

/* Banked SP: follows PSTATE.SP (SPSel) and EL, i.e. SP_EL3 at reset. */
function set_sp(value: bits(64)) -> unit = SP_set(value)

function get_sp() -> bits(64) = SP_read()

/* Packed PSTATE (LSB first):
 *   [0]=F [1]=I [2]=A [3]=D  [5:4]=BTYPE  [6]=SSBS [7]=PAN [8]=UAO
 *   [9]=DIT [10]=TCO  [12:11]=EL  [13]=SP(SPSel)  [14]=ZA [15]=SM
 * The setter writes only the maskable bits; EL/SM/ZA are read-only here
 * (change them architecturally via ERET, SMSTART/SMSTOP). */
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

function set_ffr_chunk(idx: bits(8), value: bits(64)) -> unit = {
    _FFR = Elem_set(_FFR, UInt(idx[1 .. 0]), 64, value)
}

function get_ffr_chunk(idx: bits(8)) -> bits(64) = {
    Elem_read(_FFR, UInt(idx[1 .. 0]), 64)
}

function set_zt0_chunk(idx: bits(8), value: bits(64)) -> unit = {
    _ZT0 = Elem_set(_ZT0, UInt(idx[2 .. 0]), 64, value)
}

function get_zt0_chunk(idx: bits(8)) -> bits(64) = {
    Elem_read(_ZT0, UInt(idx[2 .. 0]), 64)
}

function set_tpidr_el0(value: bits(64)) -> unit = {
    TPIDR_EL0 = value
}

function get_tpidr_el0() -> bits(64) = TPIDR_EL0

function set_tpidrro_el0(value: bits(64)) -> unit = {
    TPIDRRO_EL0 = value
}

function get_tpidrro_el0() -> bits(64) = TPIDRRO_EL0

/* After a trapping step the model vectors to EL3 (PC = VBAR_EL3+offset); these
 * read the syndrome/link/fault-address so a diff harness can classify it.
 * ESR is a bitfield register (.bits); ELR/FAR are plain bits(64). */
function get_esr_el3() -> bits(64) = ESR_EL3.bits

function get_elr_el3() -> bits(64) = ELR_EL3

function get_far_el3() -> bits(64) = FAR_EL3

function set_pc(value: bits(64)) -> unit = {
    _PC = value
}

function get_pc() -> bits(64) = {
    _PC
}

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

/* One A64 instruction: mirrors __InstructionExecute() minus the fetch (opcode
 * injected) and debug/PMU bookkeeping. */
function step_a64(opcode: bits(32)) -> unit = {
    SEE = -1;
    __BranchTaken = false;
    __ExclusiveMonitorSet = false;
    __SetThisInstrDetails(__A64, opcode, __DefaultCond(__A64));
    try {
        __DecodeExecute(__A64, opcode)
    } catch {
        exn if (IsSEE(exn) | IsUNDEFINED(exn)) | IsUNPREDICTABLE(exn) => {
            /* AArch64_UndefinedFault() itself throws Error_ExceptionTaken,
             * which this try's other arms do NOT catch (the model relies on
             * __TopLevel; we ARE the top level) — so catch it with a nested
             * try here. */
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

/* Required stub: even with --c-no-main the backend emits a model_main() that
 * calls zmain, so a Sail `main` must exist or the generated C won't compile.
 * Never called through the oracle ABI. */
function main() -> unit = ()
