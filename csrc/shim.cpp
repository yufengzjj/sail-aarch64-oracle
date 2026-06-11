/* =============================================================================
 * shim.cpp
 *
 * extern "C" ABI between Rust (src/lib.rs) and the Sail-generated C++ model
 * class (`model::Model`, produced by `sail --cpp`).
 *
 * Unlike the old `sail -c` backend (whole architectural state in process
 * globals, one model per process), the C++ backend puts every register and
 * the exception state into class members — each oracle_new() returns an
 * independent instance.
 *
 * MEMORY ISOLATION: the Sail C runtime keeps the memory model in two process
 * globals (`sail_memory` + `sail_tags` block lists in rts.c). Each instance
 * here owns its own pair of lists; the MemCtx RAII guard installs them into
 * the globals around every model call and saves the (possibly updated) heads
 * back afterwards. Single-threaded interleaving of instances is therefore
 * fully isolated, registers AND memory.
 *
 * What remains process-global is the runtime's GMP scratch temporaries
 * (refcount-initialized via setup_rts()/cleanup_rts), only live during a
 * call — so multiple instances work fine from ONE thread, but model calls
 * must not run concurrently on different threads (the Rust wrapper stays
 * !Send + !Sync).
 *
 * Naming notes (Sail C/C++ name mangling): top-level Sail identifiers get a
 * `z` prefix and any literal 'z' inside a name escapes to "zz", so the
 * harness functions set_nzcv/get_nzcv become zset_nzzcv/zget_nzzcv.
 * ===========================================================================*/

#include "sail.h"   /* unit, UNIT, fbits */
#include "model.h"  /* generated: namespace model { class Model { ... }; } */

#include <stdint.h>

/* Memory-model globals from rts.c (not exposed in rts.h). */
extern "C" {
struct block;
struct tag_block;
extern struct block *sail_memory;
extern struct tag_block *sail_tags;
void kill_mem(void);
}

namespace {

struct OracleInstance {
    model::Model m;
    struct block *memory = nullptr;   /* this instance's RAM block list  */
    struct tag_block *tags = nullptr; /* ...and tag block list           */
};

OracleInstance *cast(void *h) { return static_cast<OracleInstance *>(h); }

/* Install the instance's memory lists into the runtime globals for the
 * duration of a model call; save the heads back on exit (read/write_mem
 * prepend newly allocated blocks). Single-threaded by contract. */
class MemCtx {
    OracleInstance *inst;

public:
    explicit MemCtx(OracleInstance *i) : inst(i) {
        sail_memory = i->memory;
        sail_tags = i->tags;
    }
    ~MemCtx() {
        inst->memory = sail_memory;
        inst->tags = sail_tags;
    }
};

} // namespace

extern "C" {

/* Create a fully initialized, independent model instance.
 * model_init() refcounts the shared C runtime via setup_rts(), creates the
 * instance exception state and runs register initialization; zinit_harness
 * then performs the architectural reset (TakeReset via __InitSystem). */
void *oracle_new(void) {
    OracleInstance *o = new OracleInstance();
    MemCtx ctx(o);
    o->m.model_init();
    o->m.zinit_harness(UNIT);
    return o;
}

void oracle_free(void *h) {
    OracleInstance *o = cast(h);
    {
        MemCtx ctx(o);
        kill_mem();        /* free THIS instance's memory; leaves globals NULL */
        o->m.model_fini(); /* refcounted cleanup_rts inside (kill_mem no-ops)  */
    }
    delete o;
}

void oracle_set_gpr(void *h, uint32_t n, uint64_t value) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zset_gpr((fbits)(n & 0xff), (fbits)value);
}

uint64_t oracle_get_gpr(void *h, uint32_t n) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_gpr((fbits)(n & 0xff));
}

void oracle_set_pc(void *h, uint64_t value) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zset_pc((fbits)value);
}

uint64_t oracle_get_pc(void *h) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_pc(UNIT);
}

void oracle_set_nzcv(void *h, uint32_t flags) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zset_nzzcv((fbits)(flags & 0xf));
}

uint32_t oracle_get_nzcv(void *h) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint32_t)(o->m.zget_nzzcv(UNIT) & 0xf);
}

/* Execute a single 32-bit A64 instruction (decode + execute). */
void oracle_step(void *h, uint32_t opcode) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zstep_a64((fbits)opcode);
}

/* ---- SVE Z/P registers, 64-bit chunk granularity ---------------------------
 * Z: 32 regs x 32 chunks (2048b architectural width); P: 16 regs x 4 chunks
 * (256b). Mangling reminder: 'z' inside a Sail name escapes to "zz", so
 * set_z_chunk => zset_zz_chunk (but set_p_chunk => zset_p_chunk).           */

void oracle_set_z(void *h, uint32_t n, uint32_t chunk, uint64_t value) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zset_zz_chunk((fbits)(n & 0xff), (fbits)(chunk & 0xff), (fbits)value);
}

uint64_t oracle_get_z(void *h, uint32_t n, uint32_t chunk) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_zz_chunk((fbits)(n & 0xff), (fbits)(chunk & 0xff));
}

void oracle_set_p(void *h, uint32_t n, uint32_t chunk, uint64_t value) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zset_p_chunk((fbits)(n & 0xff), (fbits)(chunk & 0xff), (fbits)value);
}

uint64_t oracle_get_p(void *h, uint32_t n, uint32_t chunk) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_p_chunk((fbits)(n & 0xff), (fbits)(chunk & 0xff));
}

/* Current effective SVE vector length in bits (after ZCR clamping). */
uint64_t oracle_get_vl(void *h) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_vl(UNIT);
}

/* ---- FP control/status ----------------------------------------------------*/

void oracle_set_fpcr(void *h, uint64_t value) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zset_fpcr((fbits)value);
}

uint64_t oracle_get_fpcr(void *h) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_fpcr(UNIT);
}

void oracle_set_fpsr(void *h, uint64_t value) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zset_fpsr((fbits)value);
}

uint64_t oracle_get_fpsr(void *h) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_fpsr(UNIT);
}

/* ---- SME ZA storage (256 rows x 32 chunks) + SVCR --------------------------
 * Mangling: 'z' in a Sail name escapes to "zz", so set_za_chunk =>
 * zset_zza_chunk. */

void oracle_set_za(void *h, uint32_t row, uint32_t chunk, uint64_t value) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zset_zza_chunk((fbits)(row & 0xff), (fbits)(chunk & 0xff), (fbits)value);
}

uint64_t oracle_get_za(void *h, uint32_t row, uint32_t chunk) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_zza_chunk((fbits)(row & 0xff), (fbits)(chunk & 0xff));
}

/* bit1 = PSTATE.SM, bit0 = PSTATE.ZA */
uint32_t oracle_get_svcr(void *h) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint32_t)(o->m.zget_svcr(UNIT) & 0x3);
}

/* ---- stack pointer (banked: follows PSTATE.SP/EL) -------------------------*/

void oracle_set_sp(void *h, uint64_t value) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zset_sp((fbits)value);
}

uint64_t oracle_get_sp(void *h) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_sp(UNIT);
}

/* ---- packed PSTATE word (layout documented in harness.sail/lib.rs) --------*/

uint64_t oracle_get_pstate(void *h) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_pstate_bits(UNIT);
}

void oracle_set_pstate(void *h, uint64_t value) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zset_pstate_bits((fbits)value);
}

/* ---- FFR (4 chunks) / ZT0 (8 chunks; za-style zz mangling) ----------------*/

void oracle_set_ffr(void *h, uint32_t chunk, uint64_t value) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zset_ffr_chunk((fbits)(chunk & 0xff), (fbits)value);
}

uint64_t oracle_get_ffr(void *h, uint32_t chunk) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_ffr_chunk((fbits)(chunk & 0xff));
}

void oracle_set_zt0(void *h, uint32_t chunk, uint64_t value) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zset_zzt0_chunk((fbits)(chunk & 0xff), (fbits)value);
}

uint64_t oracle_get_zt0(void *h, uint32_t chunk) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_zzt0_chunk((fbits)(chunk & 0xff));
}

/* ---- thread pointers -------------------------------------------------------*/

void oracle_set_tpidr_el0(void *h, uint64_t value) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zset_tpidr_el0((fbits)value);
}

uint64_t oracle_get_tpidr_el0(void *h) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_tpidr_el0(UNIT);
}

void oracle_set_tpidrro_el0(void *h, uint64_t value) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zset_tpidrro_el0((fbits)value);
}

uint64_t oracle_get_tpidrro_el0(void *h) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_tpidrro_el0(UNIT);
}

/* ---- exception observability (read-only) -----------------------------------*/

uint64_t oracle_get_esr_el3(void *h) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_esr_el3(UNIT);
}

uint64_t oracle_get_elr_el3(void *h) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_elr_el3(UNIT);
}

uint64_t oracle_get_far_el3(void *h) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_far_el3(UNIT);
}

} // extern "C"
