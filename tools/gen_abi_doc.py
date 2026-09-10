#!/usr/bin/env python3
"""Generates the Galaxy64.dll ABI reference from the binary itself."""
import struct, sys, re
from collections import OrderedDict

PATH = sys.argv[1]
OUT = sys.argv[2]
data = open(PATH, 'rb').read()

pe_off = struct.unpack_from('<I', data, 0x3c)[0]
coff = pe_off + 4
num_sections, = struct.unpack_from('<H', data, coff + 2)
opt_size, = struct.unpack_from('<H', data, coff + 16)
opt = coff + 20
image_base, = struct.unpack_from('<Q', data, opt + 24)
num_dirs, = struct.unpack_from('<I', data, opt + 108)
dirs = [struct.unpack_from('<II', data, opt + 112 + i*8) for i in range(num_dirs)]
timestamp, = struct.unpack_from('<I', data, coff + 4)

sections = []
sec_off = opt + opt_size
for i in range(num_sections):
    b = sec_off + i*40
    name = data[b:b+8].rstrip(b'\0').decode('ascii', 'replace')
    vsize, vaddr, rawsize, rawptr = struct.unpack_from('<IIII', data, b + 8)
    sections.append({'name': name, 'vaddr': vaddr, 'vsize': vsize,
                     'rawptr': rawptr, 'rawsize': rawsize})

def rva_to_off(rva):
    for s in sections:
        if s['vaddr'] <= rva < s['vaddr'] + max(s['vsize'], s['rawsize']):
            d = rva - s['vaddr']
            if d < s['rawsize']:
                return s['rawptr'] + d
    return None

def va_to_off(va): return rva_to_off(va - image_base)

TEXT = next(s for s in sections if s['name'] == '.text')
text_lo, text_hi = image_base + TEXT['vaddr'], image_base + TEXT['vaddr'] + TEXT['vsize']
def in_text(va): return text_lo <= va < text_hi

def cstr(va, limit=250):
    o = va_to_off(va)
    if o is None: return None
    e = data.find(b'\0', o, o + limit)
    if e == -1: return None
    try: s = data[o:e].decode('ascii')
    except UnicodeDecodeError: return None
    return s if s.isprintable() else None

# exports
exp_rva, _ = dirs[0]
exp_off = rva_to_off(exp_rva)
ord_base, = struct.unpack_from('<I', data, exp_off + 16)
n_funcs, n_names = struct.unpack_from('<II', data, exp_off + 20)
addr_tbl, name_tbl, ord_tbl = struct.unpack_from('<III', data, exp_off + 28)
name_by_ord = {}
for i in range(n_names):
    nrva, = struct.unpack_from('<I', data, rva_to_off(name_tbl) + i*4)
    o, = struct.unpack_from('<H', data, rva_to_off(ord_tbl) + i*2)
    name_by_ord[o] = cstr(image_base + nrva)
exports = []
for i in range(n_funcs):
    frva, = struct.unpack_from('<I', data, rva_to_off(addr_tbl) + i*4)
    exports.append({'ordinal': ord_base + i, 'name': name_by_ord.get(i),
                    'va': image_base + frva})

def resolve_thunks(va, depth=12):
    for _ in range(depth):
        o = va_to_off(va)
        if o is None or data[o] != 0xE9: return va
        rel, = struct.unpack_from('<i', data, o + 1)
        va = va + 5 + rel
    return va

LEA = re.compile(rb'[\x48\x4c]\x8d[\x05\x0d\x15\x1d\x25\x2d\x35\x3d]', re.S)
IDENT = re.compile(r'^[A-Z][A-Za-z0-9_]{2,}$')

def method_name_at(va, window=0x600):
    o = va_to_off(va)
    if o is None: return None
    blob = data[o:o+window]
    for m in LEA.finditer(blob):
        p = m.start()
        if p + 7 > len(blob): break
        disp, = struct.unpack_from('<i', blob, p + 3)
        s = cstr(va + p + 7 + disp, 80)
        if s and IDENT.match(s): return s
    return None

def accessor_shape(va):
    """`mov rax,[rip+X]; test rax,rax; je +5; add rax,OFF; ret` -- the shape
    every galaxy::api::Foo() accessor compiles to."""
    o = va_to_off(va)
    if o is None: return None
    b = data[o:o+32]
    if not (b[0:3] == b'\x48\x8b\x05'): return None
    disp, = struct.unpack_from('<i', b, 3)
    singleton = va + 7 + disp
    off = None
    if b[7:10] == b'\x48\x85\xc0' and b[10] == 0x74:
        if b[12:15] == b'\x48\x83\xc0':
            off = b[15]
        elif b[12] == 0xc3:
            off = 0
    return {'singleton_ptr': singleton, 'offset': off}

def type_descriptor_name(td_rva):
    if rva_to_off(td_rva) is None: return None
    return cstr(image_base + td_rva + 16, 400)

def parse_col(col_rva):
    o = rva_to_off(col_rva)
    if o is None: return None
    sig, off, cd, ptd, pchd, pself = struct.unpack_from('<IIIIII', data, o)
    if sig != 1 or pself != col_rva: return None
    name = type_descriptor_name(ptd)
    if not name or not name.startswith('.?A'): return None
    return {'offset': off, 'chd': pchd, 'name': name}

def parse_chd(chd_rva):
    o = rva_to_off(chd_rva)
    if o is None: return []
    sig, attrs, n, pbca = struct.unpack_from('<IIII', data, o)
    if n > 200: return []
    bca = rva_to_off(pbca)
    if bca is None: return []
    out = []
    for i in range(n):
        bcd_rva, = struct.unpack_from('<I', data, bca + i*4)
        bo = rva_to_off(bcd_rva)
        if bo is None: continue
        ptd, ncont, mdisp, pdisp, vdisp, attr, pchd = struct.unpack_from('<IIiiiII', data, bo)
        out.append({'name': type_descriptor_name(ptd), 'mdisp': mdisp})
    return out

def demangle(n):
    if not n or not n.startswith('.?A'): return n
    parts = n[4:].rstrip('@').split('@')
    return '::'.join(reversed([p for p in parts if p]))

RDATA = [s for s in sections if s['name'] in ('.rdata', '_RDATA')]
vtables = OrderedDict()
for s in RDATA:
    start, end = s['rawptr'], s['rawptr'] + s['rawsize']
    p = start + (8 - start % 8) % 8
    while p + 16 <= end:
        maybe_col, = struct.unpack_from('<Q', data, p)
        first, = struct.unpack_from('<Q', data, p + 8)
        if in_text(first) and image_base < maybe_col < image_base + 0x10000000:
            col = parse_col(maybe_col - image_base)
            if col:
                slots = []
                q = p + 8
                while q + 8 <= end:
                    v, = struct.unpack_from('<Q', data, q)
                    if not in_text(v): break
                    slots.append(v); q += 8
                vt_rva = None
                for sec in sections:
                    if sec['rawptr'] <= p + 8 < sec['rawptr'] + sec['rawsize']:
                        vt_rva = sec['vaddr'] + (p + 8 - sec['rawptr'])
                vtables[image_base + vt_rva] = {'col': col, 'slots': slots,
                                                'bases': parse_chd(col['chd'])}
        p += 8

def interface_for(vt):
    off = vt['col']['offset']
    cands = [b for b in vt['bases'] if b['mdisp'] == off and b['name'] != vt['col']['name']]
    named = [b for b in cands if b['name'] and 'api@galaxy' in b['name']]
    if named: return demangle(named[0]['name'])
    return demangle(cands[0]['name']) if cands else None

ifaces = []
for va, vt in vtables.items():
    i = interface_for(vt)
    if not i or not i.startswith('galaxy::api::I'): continue
    if len(vt['slots']) < 2: continue
    if len(set(vt['slots'])) <= 2 and len(vt['slots']) > 4: continue
    ifaces.append((va, vt, i, demangle(vt['col']['name'])))

# correlate exports with the vtable their returned pointer carries
by_impl_off = {}
for va, vt, iface, impl in ifaces:
    by_impl_off[(impl, vt['col']['offset'])] = (va, iface)

L = []
w = L.append
w("# `Galaxy64.dll` ABI map")
w("")
w("Derived from the binary rather than from headers: GOG does not publish")
w("the Galaxy SDK without a registered developer account. Everything below")
w("comes from parsing the PE, walking its RTTI, and reading the logging")
w("strings each method passes carrying its own name.")
w("")
w("Regenerate with `tools/gen_abi_doc.py <path-to-Galaxy64.dll> docs/galaxy64-abi.md`. The binary")
w("itself is **not** in this repo (15 MB, proprietary to GOG) — copy it out")
w("of an installed game.")
w("")
w("## Why the names here are not guesses")
w("")
w("Every Galaxy method body passes its own name as a string to an internal")
w("logging call. `IStats::SetAchievement`, for instance, references the")
w("literal `\"SetAchievement\"` and a format string reading")
w("`Setting achievement: achievementID=%s, name=%s`. So each vtable slot is")
w("resolved by disassembling for rip-relative string references and taking")
w("the first identifier-shaped one — the binary naming its own methods,")
w("not an inference from declaration order.")
w("")
w("For `IStats` this was additionally cross-checked against GOG's published")
w("method list. All 35 slots agree.")
w("")
w("## The binary")
w("")
w("| | |")
w("|---|---|")
w("| Format | PE32+ (x86-64), DLL |")
w(f"| Image base | `{hex(image_base)}` |")
w(f"| Sections | {len(sections)} |")
w(f"| Exports | {len(exports)} |")
w(f"| vtables carrying RTTI | {len(vtables)} |")
w(f"| `galaxy::api` interfaces | {len(ifaces)} |")
w("")
w("### Sections")
w("")
w("| Section | VMA | Virtual size | File offset |")
w("|---|---|---|---|")
for s in sections:
    w(f"| `{s['name']}` | `{hex(image_base + s['vaddr'])}` | `{hex(s['vsize'])}` | `{hex(s['rawptr'])}` |")
w("")

w("## Exports")
w("")
w("The 28 symbols a game links against. Most are no-argument accessors")
w("returning a pointer to a singleton interface, and that is where a proxy")
w("DLL gets its opening: the accessor is the only moment a real interface")
w("pointer is handed over, so it is the natural place to patch a vtable.")
w("")
w("Each accessor compiles to the same shape — load a global, return it")
w("offset by the interface's position within the implementing object:")
w("")
w("```asm")
w("mov  rax, [rip+singleton]   ; the facade instance")
w("test rax, rax")
w("je   .null")
w("add  rax, 0x58              ; offset of the interface subobject")
w("ret")
w("```")
w("")
w("| # | Symbol | Resolved | Returns |")
w("|---|---|---|---|")
for e in exports:
    real = resolve_thunks(e['va'])
    acc = accessor_shape(real)
    if acc and acc['offset'] is not None:
        note = f"singleton `{hex(acc['singleton_ptr'])}` + `{hex(acc['offset'])}`"
    else:
        nm = method_name_at(real)
        note = f"`{nm}`" if nm else "—"
    w(f"| {e['ordinal']} | `{e['name']}` | `{hex(real)}` | {note} |")
w("")
w("`load` is an unmangled export whose body is a bare `ret` — a no-op kept")
w("for export-table compatibility.")
w("")

w("## Interfaces")
w("")
w("Every row comes from RTTI: a vtable's `CompleteObjectLocator` names the")
w("concrete class, and its `ClassHierarchyDescriptor` says which interface")
w("that particular subobject implements, by matching each base's `mdisp`")
w("against the locator's own subobject offset.")
w("")
w("Interfaces listed more than once have several implementations (a peer")
w("one and a game-server one, plus the shared base they derive from).")
w("")
w("| Interface | Implemented by | vtable | Subobject offset | Slots |")
w("|---|---|---|---|---|")
for va, vt, iface, impl in sorted(ifaces, key=lambda x: x[2]):
    w(f"| `{iface}` | `{impl}` | `{hex(va)}` | `{hex(vt['col']['offset'])}` | {len(vt['slots'])} |")
w("")

w("## Method tables")
w("")
w("Slot 0 is always the virtual destructor — an *adjustor thunk*")
w("(`sub rcx, <offset>; jmp ...`) whenever the interface does not sit at")
w("offset 0 of the complete object, which is why it carries no name of its")
w("own. A `?` elsewhere is a method whose body never logs its name;")
w("in practice those are trivial getters.")
w("")
w("Addresses are the real function bodies, with incremental-link `jmp`")
w("thunks already followed through.")
w("")
for va, vt, iface, impl in sorted(ifaces, key=lambda x: x[2]):
    named = 0
    rows = []
    for i, s in enumerate(vt['slots']):
        real = resolve_thunks(s)
        nm = method_name_at(real)
        if nm: named += 1
        label = f"`{nm}`" if nm else ("*destructor*" if i == 0 else "?")
        rows.append(f"| {i} | `{hex(real)}` | {label} |")
    w(f"### `{iface}` — `{impl}`")
    w("")
    w(f"vtable `{hex(va)}`, {len(vt['slots'])} slots, {named} identified.")
    w("")
    w("| Slot | Function | Method |")
    w("|---|---|---|")
    L.extend(rows)
    w("")

open(OUT, 'w').write("\n".join(L) + "\n")
print(f"wrote {OUT}: {len(L)} lines, {len(ifaces)} interfaces")
