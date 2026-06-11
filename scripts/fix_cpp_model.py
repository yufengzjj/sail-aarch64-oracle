#!/usr/bin/env python3
"""Fix Sail 0.20.1 `--cpp` backend output (run on the generated model.h).

The C++ backend has three rough edges on large models like arm-v9.4-a:

1. Some method declarations are emitted TWICE inside the class body
   (`zeq_anyzIE...` equality helpers). Duplicate prototypes are valid C but
   an error in a C++ class. -> drop exact duplicate declaration lines.

2. A set of small prelude functions is declared in the class and called from
   the generated code, but their definitions are never emitted (the C backend
   inlines them; the C++ backend loses them): the per-enum `zeq_anyzIE<E>z5zK`
   equality helpers plus zUInt0 / zediv_nat / zemod_nat / zappend_str /
   zputchar / zeq_anyzIrzK. -> generate `model_missing.cpp` next to model.h
   with the obvious bodies (skipping any that model.cpp does define).

3. It emits calls to `sint` / `sub_vec_int` where the C backend emits
   `sail_signed` / `sub_bits_int`. Handled by inline aliases in
   vendor/sail-runtime/sail.h (CPP-BACKEND PATCH), not here.

4. The ~250k GMP scratch temporaries (zghz3*) are FILE-SCOPE GLOBALS shared
   by all instances, yet every instance's model_init()/model_fini() runs the
   startup_z*()/finish_z*() calls that CREATE/KILL them — so destroying one
   instance frees the scratch out from under the others (use-after-free).
   -> patch model.cpp to run the startup block only for the first live
   instance and the finish block only for the last one (refcounted), while
   keeping the per-instance parts (exception state, let-bindings, register
   initialization, member KILLs) untouched.

Usage: fix_cpp_model.py path/to/model.h path/to/model.cpp
"""
import os
import re
import sys

# name -> (declaration regex on model.h line, body template)
SPECIALS = {
    "zUInt0": (
        r"void zUInt0\(sail_int \*rop, lbits\);",
        "void Model::zUInt0(sail_int *rop, lbits op)\n"
        "{\n  sail_unsigned(rop, op);\n}\n",
    ),
    "zediv_nat": (
        r"void zediv_nat\(sail_int \*rop, sail_int, sail_int\);",
        "void Model::zediv_nat(sail_int *rop, sail_int op1, sail_int op2)\n"
        "{\n  ediv_int(rop, op1, op2);\n}\n",
    ),
    "zemod_nat": (
        r"void zemod_nat\(sail_int \*rop, sail_int, sail_int\);",
        "void Model::zemod_nat(sail_int *rop, sail_int op1, sail_int op2)\n"
        "{\n  emod_int(rop, op1, op2);\n}\n",
    ),
    "zappend_str": (
        r"void zappend_str\(sail_string \*rop, const_sail_string, const_sail_string\);",
        "void Model::zappend_str(sail_string *rop, const_sail_string op1, const_sail_string op2)\n"
        "{\n  concat_str(rop, op1, op2);\n}\n",
    ),
    "zputchar": (
        r"unit zputchar\(sail_int\);",
        "unit Model::zputchar(sail_int op)\n"
        "{\n  return sail_putchar(op);\n}\n",
    ),
    "zeq_anyzIrzK": (
        r"bool zeq_anyzIrzK\(real, real\);",
        "bool Model::zeq_anyzIrzK(real op1, real op2)\n"
        "{\n  return EQUAL(real)(op1, op2);\n}\n",
    ),
}

ENUM_EQ = re.compile(r"^\s+bool (zeq_anyzIE[A-Za-z_0-9]+z5zK)\(enum (z[A-Za-z_0-9]+), enum \2\);\s*$")


def dedup_header(path: str) -> list[str]:
    """Drop duplicate declaration lines in class bodies; return kept lines."""
    seen = set()
    out = []
    dropped = 0
    in_class = False
    for line in open(path):
        if line.startswith("class "):
            in_class = True
        elif line.startswith("};"):
            in_class = False
            seen.clear()
        if in_class and line.rstrip().endswith(");"):
            if line in seen:
                dropped += 1
                continue
            seen.add(line)
        out.append(line)
    with open(path, "w") as f:
        f.writelines(out)
    print(f"{path}: dropped {dropped} duplicate declaration(s)")
    return out


def gen_missing(header_lines: list[str], header_path: str, cpp_path: str) -> None:
    # Names that model.cpp DOES define (scan definition lines only, cheap).
    defined = set()
    defn = re.compile(r"^[a-z][a-z_0-9 ]*\**\s*Model::(z[A-Za-z_0-9]+)\(")
    for line in open(cpp_path):
        m = defn.match(line)
        if m:
            defined.add(m.group(1))

    bodies = []
    declared_specials = set()
    for line in header_lines:
        m = ENUM_EQ.match(line)
        if m and m.group(1) not in defined:
            name, ty = m.group(1), m.group(2)
            bodies.append(
                f"bool Model::{name}(enum {ty} op1, enum {ty} op2)\n"
                f"{{\n  return op1 == op2;\n}}\n"
            )
            defined.add(name)  # tolerate dup declarations
            continue
        for name, (decl_re, body) in SPECIALS.items():
            if name not in declared_specials and re.search(decl_re, line):
                declared_specials.add(name)
                if name not in defined:
                    bodies.append(body)

    out_path = os.path.join(os.path.dirname(header_path), "model_missing.cpp")
    with open(out_path, "w") as f:
        f.write(
            "// Generated by scripts/fix_cpp_model.py — definitions for methods the\n"
            "// Sail 0.20.1 --cpp backend declares (and calls) but never emits.\n"
            '#include "sail.h"\n#include "sail_config.h"\n#include "rts.h"\n'
            '#include "model.h"\n\nnamespace model {\n\n'
        )
        f.write("\n".join(bodies))
        f.write("\n} // namespace model\n")
    print(f"{out_path}: generated {len(bodies)} missing definition(s)")


STARTUP = re.compile(r"^\s+startup_z[A-Za-z_0-9]+\(\);\s*$")
FINISH = re.compile(r"^\s+finish_z[A-Za-z_0-9]+\(\);\s*$")
GUARD_DECL = "static int sail_scratch_refs = 0; /* fix_cpp_model.py */\n"


def refcount_scratch(cpp_path: str) -> None:
    """Wrap global-scratch startup/finish call runs in first/last-instance guards."""
    tmp_path = cpp_path + ".tmp"
    in_fn = None  # None | "init" | "fini"
    runs_wrapped = 0
    already = False
    with open(cpp_path) as src, open(tmp_path, "w") as dst:
        pending = []  # buffered run of startup/finish lines

        def flush(cond: str) -> None:
            nonlocal runs_wrapped
            if pending:
                dst.write(f"  if ({cond}) {{ /* fix_cpp_model.py */\n")
                dst.writelines(pending)
                dst.write("  }\n")
                pending.clear()
                runs_wrapped += 1

        for line in src:
            if "fix_cpp_model.py" in line:
                already = True
            if in_fn is None:
                if line.startswith("void Model::model_init(void)"):
                    in_fn = "init"
                    dst.write(GUARD_DECL + line + "{\n")
                    dst.write("  const bool sail_first = (sail_scratch_refs++ == 0);\n")
                    dst.write("  (void)sail_first;\n")
                    continue
                if line.startswith("void Model::model_fini(void)"):
                    in_fn = "fini"
                    dst.write(line + "{\n")
                    dst.write("  const bool sail_last = (--sail_scratch_refs == 0);\n")
                    dst.write("  (void)sail_last;\n")
                    continue
                dst.write(line)
                continue
            # inside model_init / model_fini
            if line.startswith("{"):
                continue  # opening brace already emitted
            pat = STARTUP if in_fn == "init" else FINISH
            cond = "sail_first" if in_fn == "init" else "sail_last"
            if pat.match(line):
                pending.append(line)
                continue
            flush(cond)
            dst.write(line)
            if line.startswith("}"):
                in_fn = None
    if already:
        os.remove(tmp_path)
        print(f"{cpp_path}: scratch refcount already applied, skipped")
        return
    os.replace(tmp_path, cpp_path)
    print(f"{cpp_path}: wrapped {runs_wrapped} startup/finish run(s) in refcount guards")


if __name__ == "__main__":
    header, cpp = sys.argv[1], sys.argv[2]
    kept = dedup_header(header)
    gen_missing(kept, header, cpp)
    refcount_scratch(cpp)
