/* =============================================================================
 * shim.cpp — extern "C" ABI between Rust and the Sail C++ model class
 * (`model::Model`, from `sail --cpp`; architectural state lives in members, so
 * each oracle_new() is independent).
 *
 * MEMORY ISOLATION: the runtime keeps memory in process globals (sail_memory /
 * sail_tags block lists, rts.c). Each instance owns its own pair; the MemCtx
 * RAII guard swaps them into the globals around every call. Correct only
 * single-threaded — the Rust wrapper serializes all calls under one mutex.
 *
 * NAME MANGLING: Sail prefixes top-level names with `z` and escapes a literal
 * 'z' to "zz", so e.g. set_nzcv -> zset_nzzcv, set_za_chunk -> zset_zza_chunk.
 * ===========================================================================*/

#include "sail.h"
#include "model.h"

#include <stdint.h>

/* Memory-model globals + accessors from rts.c (not exposed in rts.h).
 * read/write_mem are byte-addressed, little-endian (byte at `address` = LSB)
 * and are exactly what the model's loads/stores decompose into. */
extern "C" {
struct block;
struct tag_block;
extern struct block *sail_memory;
extern struct tag_block *sail_tags;
void kill_mem(void);
uint64_t read_mem(uint64_t address);
void write_mem(uint64_t address, uint64_t byte);
bool sail_addr_mapped(uint64_t address);
}

namespace {

struct OracleInstance {
    model::Model m;
    struct block *memory = nullptr;
    struct tag_block *tags = nullptr;
};

OracleInstance *cast(void *h) { return static_cast<OracleInstance *>(h); }

/* Swap the instance's memory lists into the runtime globals for the call, save
 * the (possibly grown) heads back on exit. Single-threaded by contract. */
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

void *oracle_new(void) {
    OracleInstance *o = new OracleInstance();
    MemCtx ctx(o);
    o->m.model_init();           // setup_rts() refcounted; zinit_harness resets
    o->m.zinit_harness(UNIT);
    return o;
}

void oracle_free(void *h) {
    OracleInstance *o = cast(h);
    {
        MemCtx ctx(o);
        kill_mem();        // frees THIS instance's memory
        o->m.model_fini(); // refcounted cleanup_rts inside
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

void oracle_step(void *h, uint32_t opcode) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    o->m.zstep_a64((fbits)opcode);
}

/* set_z_chunk => zset_zz_chunk (zz mangling), but set_p_chunk => zset_p_chunk. */

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

uint64_t oracle_get_vl(void *h) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint64_t)o->m.zget_vl(UNIT);
}

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

/* set_za_chunk => zset_zza_chunk (zz mangling). */
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

uint32_t oracle_get_svcr(void *h) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint32_t)(o->m.zget_svcr(UNIT) & 0x3);
}

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

/* set_zt0_chunk => zset_zzt0_chunk (zz mangling). */
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

uint8_t oracle_read_mem(void *h, uint64_t addr) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return (uint8_t)read_mem(addr);
}

void oracle_write_mem(void *h, uint64_t addr, uint8_t byte) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    write_mem(addr, (uint64_t)byte);
}

bool oracle_is_mapped(void *h, uint64_t addr) {
    OracleInstance *o = cast(h);
    MemCtx ctx(o);
    return sail_addr_mapped(addr);
}

} // extern "C"
