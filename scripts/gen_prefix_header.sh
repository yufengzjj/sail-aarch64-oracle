#!/bin/sh
# Write csrc/sailarm_prefix.h from the global names the built static libs
# define, or with --check fail if a lib still defines a global name that is
# not `sailarm_*` (C++ names in namespace sail_aarch64 or std are fine).
#
# Usage: scripts/gen_prefix_header.sh [--check]
# Runs `cargo build`, then reads the libs with llvm-objdump (LLVM_OBJDUMP,
# else PATH, else C:\LLVM\bin). MSVC (COFF) objects only. Names the current
# header already renames are read back without the prefix, so regenerating
# after a Sail upgrade keeps the old names and adds the new ones. After
# regenerating, rebuild and run --check.
#
# The model must already use namespace sail_aarch64 (scripts/fix_cpp_model.py):
# under the default `model`, this would write `#define model sailarm_model`.
set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
header=$here/csrc/sailarm_prefix.h
case ${1:-} in
    "") check=0 ;;
    --check) check=1 ;;
    *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

objdump=${LLVM_OBJDUMP:-$(command -v llvm-objdump || true)}
if [ -z "$objdump" ] && [ -x /c/LLVM/bin/llvm-objdump.exe ]; then
    objdump=/c/LLVM/bin/llvm-objdump.exe
fi
[ -n "$objdump" ] || { echo "llvm-objdump not found; set LLVM_OBJDUMP" >&2; exit 1; }

[ -f "$header" ] || printf '#pragma once\n' > "$header"

out_dir=$(cd "$here" && cargo build --message-format=json-render-diagnostics |
    grep '"reason":"build-script-executed"' | grep 'sail-aarch64-oracle#' |
    sed 's/.*"out_dir":"\([^"]*\)".*/\1/; s/\\\\/\//g' | tail -n 1)
[ -n "$out_dir" ] || { echo "cargo build failed or printed no out_dir" >&2; exit 1; }

# One line per external symbol: "<lib> <object> <kind> <token>".
# kind: strong, any (COMDAT the linker may share: inline code, literals),
# common, undef. token: the name a #define must rename, i.e. the C name, or
# for an MSVC-mangled C++ name its global name / outermost scope; "-" for
# templates, string literals, RTTI and local-scope names.
list_symbols='
function token(s,   rest, c, last) {
    if (s !~ /^\?/) return s
    rest = substr(s, 2)
    if (rest ~ /^\?\$/ || rest ~ /^\?_[CR]/) return "-"
    if (rest ~ /^\?__/) rest = substr(rest, 5)
    else if (rest ~ /^\?_/) rest = substr(rest, 4)
    else if (rest ~ /^\?/) rest = substr(rest, 3)
    else {
        c = index(rest, "@"); if (!c) return "-"
        last = substr(rest, 1, c - 1); rest = substr(rest, c + 1)
    }
    while (rest != "" && substr(rest, 1, 1) != "@") {
        if (rest ~ /^[?0-9]/) return "-"
        c = index(rest, "@"); if (!c) return "-"
        last = substr(rest, 1, c - 1); rest = substr(rest, c + 1)
    }
    return last == "" ? "-" : last
}
function flush(   i, k) {
    for (i = 1; i <= n; i++) {
        k = sel[dsec[i]]
        if (k == 5) k = sel[assoc[dsec[i]]]
        print lib, obj, (k >= 2 && k <= 6 ? "any" : "strong"), token(dname[i])
    }
    n = 0; split("", sel); split("", assoc)
}
/^[^[].*:\tfile format / {
    flush()
    if ($0 !~ /file format coff/) { print "not a COFF object: " $0 > "/dev/stderr"; exit 1 }
    obj = $0; sub(/^[^(]*\(/, "", obj); sub(/\):\t.*/, "", obj)
    next
}
/^\[/ {
    match($0, /\(sec *-?[0-9]+\)/); sec = substr($0, RSTART + 4, RLENGTH - 5) + 0
    match($0, /\(scl *[0-9]+\)/); scl = substr($0, RSTART + 4, RLENGTH - 5) + 0
    match($0, /\(nx *[0-9]+\)/); nx = substr($0, RSTART + 3, RLENGTH - 4) + 0
    pend = (scl == 3 && nx == 1) ? sec : -1
    if (scl != 2) next
    if (sec > 0) { dsec[++n] = sec; dname[n] = $NF }
    else if (sec == 0) print lib, obj, ($(NF - 1) ~ /^0x0+$/ ? "undef" : "common"), token($NF)
    next
}
/^AUX scnlen/ && pend >= 0 {
    for (i = 1; i < NF; i++) {
        if ($i == "comdat") sel[pend] = $(i + 1) + 0
        if ($i == "assoc") assoc[pend] = $(i + 1) + 0
    }
    pend = -1
}
END { flush() }
'

symbols=$(
    for lib in sailarm_model sailarm_runtime; do
        file=$out_dir/$lib.lib
        [ -f "$file" ] || { echo "missing $file" >&2; exit 1; }
        "$objdump" -t "$file" | awk -v lib="$lib" "$list_symbols" || exit 1
    done
)

# The C library's inline stdio functions and compiler-made names (leading _)
# are shared COMDAT code, not ours to rename.
crt_inline='$3 == "any" && ($4 ~ /^_/ || $4 ~ /^v?(s|sn|f)?(printf|scanf)$/)'

if [ "$check" = 1 ]; then
    bad=$(printf '%s\n' "$symbols" | awk "
        \$3 == \"undef\" || \$4 == \"-\" || \$4 == \"sail_aarch64\" || \$4 == \"std\" || \$4 ~ /^sailarm_/ { next }
        $crt_inline { next }
        { print \$1 \": \" \$4 \" (\" \$3 \", \" \$2 \")\" }" | sort -u)
    holes=$(printf '%s\n' "$symbols" | awk '
        FNR == NR { if ($1 == "#define" && $3 == "sailarm_" $2) want[$2] = 1; next }
        $3 == "undef" && ($4 in want) { print $1 ": " $4 " (" $2 ")" }' "$header" - | sort -u)
    total=$(printf '%s\n' "$symbols" | awk '$3 != "undef"' | wc -l)
    names=$(grep -c '^#define .* sailarm_' "$header" || true)
    status=0
    if [ -n "$bad" ]; then
        echo "Global names without the sailarm_ prefix:"
        printf '%s\n' "$bad"
        status=1
    fi
    if [ -n "$holes" ]; then
        echo "References to names the header renames, left unrenamed:"
        printf '%s\n' "$holes"
        status=1
    fi
    [ "$status" = 1 ] || echo "OK: $total defined external symbols checked, $names names renamed"
    exit "$status"
fi

# Group: 1 model and shim, 2 Sail runtime, 3 mini-gmp.
picked=$(printf '%s\n' "$symbols" | awk "
    \$3 == \"undef\" || \$4 == \"-\" || \$4 == \"sail_aarch64\" || \$4 == \"std\" { next }
    $crt_inline { next }
    {
        name = \$4
        if (name ~ /^sailarm_/) {
            if (\$2 ~ /shim\\.o/) next
            name = substr(name, 9)
        }
        if (\$1 == \"sailarm_model\") g = 1
        else if (\$2 ~ /mini-(gmp|mpq)\\.o/) g = 3
        else g = 2
        print g, name
    }" | LC_ALL=C sort -k2,2 -k1,1n | awk '!seen[$2]++' | LC_ALL=C sort -k1,1n -k2,2)

# gmp.h is only included with SAIL_SYSTEM_GMP.
model_cpp=${AARCH64_ORACLE_SAIL_MODEL_C:-$out_dir/model.cpp}
cxx_headers=$(cd "$here" && cat csrc/*.cpp vendor/*.h vendor/model_missing.cpp \
    vendor/sail-runtime/*.h "$model_cpp" |
    sed -n 's/^[[:space:]]*#[[:space:]]*include[[:space:]]*<\([a-z_./]*\)>.*/\1/p' |
    grep -v '^gmp\.h$' | LC_ALL=C sort -u)

{
    echo "/* Generated by scripts/gen_prefix_header.sh; force-included into every C/C++ file. */"
    echo "#ifndef SAILARM_PREFIX_H"
    echo "#define SAILARM_PREFIX_H"
    if [ -n "$cxx_headers" ]; then
        echo
        echo "#ifdef __cplusplus"
        echo "/* Included before the renames, which would otherwise also rename std members such as"
        echo "   std::string::append (an undefined symbol where libstdc++ instantiates it). */"
        printf '#include <%s>\n' $cxx_headers
        echo "#endif"
    fi
    printf '%s\n' "$picked" | awk '
        $1 != g {
            if (g == 3) print "#endif"
            print ""
            if ($1 == 3) {
                print "/* With SAIL_SYSTEM_GMP, gmp.h maps these names to its own __gmp* symbols. */"
                print "#ifndef SAIL_SYSTEM_GMP"
            }
            g = $1
        }
        { print "#define " $2 " sailarm_" $2 }
        END { if (g == 3) print "#endif" }'
    echo
    echo "#endif"
} > "$header"

printf '%s\n' "$picked" | awk '
    BEGIN { split("model and shim,Sail runtime,mini-gmp", label, ",") }
    { c[$1]++ }
    END { for (g = 1; g <= 3; g++) printf "%-15s %d\n", label[g] ":", c[g] }'
echo "Wrote $header; rebuild and run $0 --check."
