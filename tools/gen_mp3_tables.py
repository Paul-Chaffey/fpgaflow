#!/usr/bin/env python3
"""Generate crates/mp3model/src/tables.rs from two public-domain MP3 decoders.

    python3 -I tools/gen_mp3_tables.py MINIMP3_H PDMP3_C OUT_RS

Sources (both public domain; neither file is compiled or run, only the text
of their array literals is parsed):
  minimp3.h  https://github.com/lieff/minimp3   (CC0)
  pdmp3.c    https://github.com/technosaurus/PDMP3 (Unlicense)

The two store the Huffman tables in different forms: minimp3 as multi-level
lookup tables, PDMP3 as ISO-reference binary trees. Both are expanded to
explicit (code, length, x, y) lists and must agree exactly; every table must
be prefix-free and complete (Kraft sum 1) with the expected number of entries.
The scale factor band edges are cross-checked the same way.
"""
import re
import sys
from fractions import Fraction


def array_text(src, name):
    """The brace-enclosed initialiser of `name[...] = { ... }`, comments removed."""
    m = re.search(r'\b' + re.escape(name) + r'\s*(\[[^\]]*\])+\s*=\s*\{', src)
    if not m:
        sys.exit(f"array {name} not found")
    i, depth = m.end() - 1, 0
    for j in range(i, len(src)):
        if src[j] == '{':
            depth += 1
        elif src[j] == '}':
            depth -= 1
            if depth == 0:
                return src[i:j + 1]
    sys.exit(f"array {name} not terminated")


def strip_comments(src):
    src = re.sub(r'/\*.*?\*/', '', src, flags=re.S)
    return re.sub(r'//[^\n]*', '', src)


def numbers(text):
    out = []
    for t in re.findall(r'-?0x[0-9a-fA-F]+|-?\d+\.\d*(?:[eE][-+]?\d+)?f?|-?\d+', text):
        if 'x' in t:
            out.append(int(t, 16))
        elif '.' in t:
            out.append(float(t.rstrip('f')))    # C float suffix (never on hex)
        else:
            out.append(int(t))
    return out


def check_code(name, codes, expect_n):
    """Prefix-free, complete, expected size."""
    if len(codes) != expect_n:
        sys.exit(f"{name}: {len(codes)} codes, expected {expect_n}")
    if len({v for _, _, v in codes}) != expect_n:
        sys.exit(f"{name}: some values decode from more than one code")
    kraft = sum(Fraction(1, 2 ** n) for n, _, _ in codes)
    if kraft != 1:
        sys.exit(f"{name}: Kraft sum {kraft}, not 1")
    bits = sorted(format(c, f'0{n}b') for n, c, _ in codes)
    for a, b in zip(bits, bits[1:]):
        if b.startswith(a):
            sys.exit(f"{name}: {a} is a prefix of {b}")


# ---------------------------------------------------------------- minimp3
def minimp3_pairs(tabs, base0):
    """Walk one minimp3 codebook: {(len, code): (x, y)}."""
    out = {}

    def walk(base, w, prefix, plen):
        for i in range(1 << w):
            leaf = tabs[base0 + base + i]
            if leaf >= 0:
                n = leaf >> 8
                if i & ((1 << (w - n)) - 1):
                    continue                      # same leaf, other don't-care bits
                code = (prefix << n) | (i >> (w - n))
                out[(plen + n, code)] = (leaf & 15, (leaf >> 4) & 15)
            else:
                walk(-(leaf >> 3), leaf & 7, (prefix << w) | i, plen + w)

    walk(0, 5, 0, 0)
    return out


def minimp3_count1(tab):
    out = {}
    for i in range(16):
        leaf = tab[i]
        if leaf & 8:
            n = leaf & 7
            if i & ((1 << (4 - n)) - 1):
                continue
            out[(n, i >> (4 - n))] = leaf >> 4
        else:
            k, base = leaf & 3, leaf >> 3
            for j in range(1 << k):
                leaf2 = tab[base + j]
                n = leaf2 & 7                     # total length
                full = (i << k) | j
                if full & ((1 << (4 + k - n)) - 1):
                    continue
                out[(n, full >> (4 + k - n))] = leaf2 >> 4
    return out


# ---------------------------------------------------------------- PDMP3
def pdmp3_tree(t, start, size):
    """Walk one ISO-reference tree: {(len, code): value byte}."""
    out = {}

    def child(point, bit):
        sh = 8 if bit == 0 else 0
        while ((t[start + point] >> sh) & 0xff) >= 250:
            point += (t[start + point] >> sh) & 0xff
        return point + ((t[start + point] >> sh) & 0xff)

    def walk(point, code, n):
        if point >= size:
            sys.exit(f"tree at {start}: node {point} outside {size}")
        node = t[start + point]
        if node & 0xff00 == 0:
            out[(n, code)] = node & 0xff
            return
        walk(child(point, 0), code << 1, n + 1)
        walk(child(point, 1), (code << 1) | 1, n + 1)

    walk(0, 0, 0)
    return out


def main():
    mm = strip_comments(open(sys.argv[1], encoding='utf-8').read())
    pd_raw = open(sys.argv[2], encoding='utf-8').read()
    pd = strip_comments(pd_raw)

    tabs = numbers(array_text(mm, 'tabs'))
    tabindex = numbers(array_text(mm, 'tabindex'))
    linbits = numbers(array_text(mm, 'g_linbits'))
    tab32 = numbers(array_text(mm, 'tab32'))
    tab33 = numbers(array_text(mm, 'tab33'))
    ht = numbers(array_text(pd, 'g_huffman_table'))

    # PDMP3's table index: (offset, length, linbits) per table 0..33.
    main_tab = re.findall(r'\{\s*(NULL|g_huffman_table\s*(?:\+\s*\d+)?)\s*,\s*(\d+)\s*,\s*(\d+)\s*\}',
                          array_text(pd, 'g_huffman_main'))
    if len(main_tab) != 34:
        sys.exit(f"g_huffman_main: {len(main_tab)} entries")

    sizes = {1: 4, 2: 9, 3: 9, 5: 16, 6: 16, 7: 36, 8: 36, 9: 36, 10: 64, 11: 64, 12: 64,
             13: 256, 15: 256}
    pairs = {}
    for tn in range(32):
        off, length, lb = main_tab[tn]
        if off == 'NULL':
            if tn not in (0, 4, 14):
                sys.exit(f"table {tn} missing in PDMP3")
            continue
        start = int(off.split('+')[1]) if '+' in off else 0
        a = {k: (v >> 4, v & 15) for k, v in pdmp3_tree(ht, start, int(length)).items()}
        b = minimp3_pairs(tabs, tabindex[tn])
        if a != b:
            diff = set(a.items()) ^ set(b.items())
            sys.exit(f"table {tn}: sources disagree ({len(diff)} entries), e.g. {sorted(diff)[:4]}")
        if int(lb) != linbits[tn]:
            sys.exit(f"table {tn}: linbits {lb} vs {linbits[tn]}")
        codes = [(n, c, xy) for (n, c), xy in a.items()]
        check_code(f"table {tn}", codes, sizes.get(tn, 256))
        pairs[tn] = sorted(codes)

    # Count1: A from both; B is plain 4-bit codes. PDMP3's index entry for
    # table 33 points into table 24 (a bug there), so take B from the data
    # that follows table 32 (offset of 32 + its length).
    off32 = int(main_tab[32][0].split('+')[1])
    count1 = []
    for k, tab in ((0, tab32), (1, tab33)):
        start = off32 + 31 * k
        a = pdmp3_tree(ht, start, 31)
        b = minimp3_count1(tab)
        if a != b:
            sys.exit(f"count1 table {'AB'[k]}: sources disagree")
        codes = [(n, c, v) for (n, c), v in a.items()]
        check_code(f"count1 {'AB'[k]}", codes, 16)
        count1.append(sorted(codes))
    if any(n != 4 or c != 15 - v for n, c, v in count1[1]):
        sys.exit("count1 B is not the inverted 4-bit code")

    # Scale factor band edges, MPEG-1, [44.1, 48, 32] kHz.
    sfb = numbers(array_text(pd, 'g_sf_band_indices'))
    long_edges, short_edges = [], []
    for r in range(3):
        row = sfb[r * 37:(r + 1) * 37]
        long_edges.append(row[:23])
        short_edges.append(row[23:])
    scf_long = numbers(array_text(mm, 'g_scf_long'))
    scf_short = numbers(array_text(mm, 'g_scf_short'))
    for r in range(3):
        widths = scf_long[(5 + r) * 23:(6 + r) * 23][:22]
        e = [0]
        for w in widths:
            e.append(e[-1] + w)
        if e != long_edges[r]:
            sys.exit(f"long band edges, rate {r}: {e} vs {long_edges[r]}")
        sw = scf_short[(5 + r) * 40:(6 + r) * 40][:39:3]
        e = [0]
        for w in sw:
            e.append(e[-1] + w)
        if e != short_edges[r]:
            sys.exit(f"short band edges, rate {r}: {e} vs {short_edges[r]}")

    slen = numbers(array_text(pd, 'mpeg1_scalefac_sizes'))
    pretab = numbers(re.search(r'pretab\[21\]\s*=\s*\{([^}]*)\}', pd).group(1))
    dtbl = numbers(array_text(pd, 'g_synth_dtbl'))
    if len(dtbl) != 512:
        sys.exit(f"synthesis window: {len(dtbl)} values")
    d_int = [round(v * 65536) for v in dtbl]
    if any(abs(v * 65536 - i) > 0.01 for v, i in zip(dtbl, d_int)):
        sys.exit("synthesis window values are not multiples of 2^-16")

    # ---------------------------------------------------------------- emit
    o = []
    w = o.append
    w("//! MPEG-1 Layer III tables. Generated by tools/gen_mp3_tables.py from")
    w("//! minimp3 (CC0) and PDMP3 (Unlicense), which agree on every Huffman code")
    w("//! and band edge; do not edit by hand.")
    w("")
    w("/// One Huffman code: `len` bits, value `code` (MSB first), decoding to")
    w("/// the pair (x, y) for big-value tables or the flags v w x y (bits 3..0)")
    w("/// for the count1 tables.")
    w("#[derive(Debug, Clone, Copy, PartialEq, Eq)]")
    w("pub struct Code {")
    w("    pub len: u8,")
    w("    pub code: u16,")
    w("    pub x: u8,")
    w("    pub y: u8,")
    w("}")
    w("")
    w("const fn c(len: u8, code: u16, x: u8, y: u8) -> Code {")
    w("    Code { len, code, x, y }")
    w("}")
    w("")
    emitted = {}
    for tn in sorted(pairs):
        key = tuple(pairs[tn])
        if key in emitted:
            continue
        emitted[key] = f"PAIRS_{tn}"
        w(f"static PAIRS_{tn}: [Code; {len(key)}] = [")
        for n, cd, (x, y) in key:
            w(f"    c({n}, 0b{cd:0{n}b}, {x}, {y}),")
        w("];")
    w("")
    w("/// Big-value Huffman tables 0..31 (0, 4 and 14 are empty; 16-23 and 24-31")
    w("/// share a code and differ in linbits).")
    w("pub static PAIRS: [&[Code]; 32] = [")
    for tn in range(32):
        w(f"    &{emitted[tuple(pairs[tn])]}," if tn in pairs else "    &[],")
    w("];")
    w("")
    w(f"pub const LINBITS: [u8; 32] = {linbits};")
    w("")
    w("/// Count1 (quadruple) tables A and B.")
    w("pub static COUNT1: [[Code; 16]; 2] = [")
    for codes in count1:
        w("    [")
        for n, cd, v in codes:
            w(f"        c({n}, 0b{cd:0{n}b}, {v}, 0),")
        w("    ],")
    w("];")
    w("")
    w("/// Long-block scale factor band edges (22 bands), for [44.1, 48, 32] kHz.")
    w(f"pub const SFB_LONG: [[u16; 23]; 3] = {long_edges};")
    w("/// Short-block band edges (13 bands, per window), for [44.1, 48, 32] kHz.")
    w(f"pub const SFB_SHORT: [[u16; 14]; 3] = {short_edges};")
    w("")
    w("/// (slen1, slen2) for each scalefac_compress value.")
    pairs_s = ", ".join(f"({slen[2*i]}, {slen[2*i+1]})" for i in range(16))
    w(f"pub const SLEN: [(u8, u8); 16] = [{pairs_s}];")
    w("")
    w(f"pub const PRETAB: [u8; 22] = {pretab + [0]};")
    w("")
    w("/// Synthesis window D[i] x 2^16 (ISO 11172-3 table B.3; exact integers).")
    w("pub const SYNTH_D: [i32; 512] = [")
    for i in range(0, 512, 16):
        w("    " + ", ".join(str(v) for v in d_int[i:i + 16]) + ",")
    w("];")
    open(sys.argv[3], 'w', encoding='utf-8').write("\n".join(o) + "\n")
    print(f"tables OK: {len(pairs)} big-value tables ({len(emitted)} distinct codes), "
          f"2 count1, band edges for 3 rates, window D[512]; wrote {sys.argv[3]}")


main()
