#!/usr/bin/env python3
"""Generates the Galaxy64.dll ABI reference from the binary itself."""
import bisect, hashlib, struct, sys, re
from collections import OrderedDict, defaultdict

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

def method_name_at(va):
    """First identifier-shaped string referenced inside the function itself: the
    scan stops at the end of the function (`.pdata`), otherwise a short body would
    pick up a string from whatever follows it."""
    lo, hi = func_range(va) or (va, va + 0x100)
    for site, tgt in refs_in(lo, hi):
        s = cstr(tgt, 80)
        if s and IDENT.match(s): return s
    return None

pdata = next(s for s in sections if s['name'] == '.pdata')
unwind = sorted(struct.unpack_from('<III', data, pdata['rawptr'] + 12 * i) for i in range(pdata['vsize'] // 12))
unwind_begin = [b for b, _, _ in unwind]

def func_range(va):
    i = bisect.bisect_right(unwind_begin, va - image_base) - 1
    if i >= 0 and unwind[i][0] <= va - image_base < unwind[i][1]:
        return image_base + unwind[i][0], image_base + unwind[i][1]
    return None

# every rip-relative `lea`, so "who references this string" and "what does this
# function reference" are both bisect lookups
lea_sites, lea_targets = [], []
for m in LEA.finditer(data, TEXT['rawptr'], TEXT['rawptr'] + TEXT['vsize']):
    site = text_lo + (m.start() - TEXT['rawptr'])
    disp, = struct.unpack_from('<i', data, m.start() + 3)
    lea_sites.append(site)
    lea_targets.append(site + 7 + disp)

def refs_in(lo, hi):
    i = bisect.bisect_left(lea_sites, lo)
    while i < len(lea_sites) and lea_sites[i] < hi:
        yield lea_sites[i], lea_targets[i]
        i += 1

def string_va(text):
    o = data.find(b'\0' + text.encode() + b'\0')
    if o == -1: return None
    for s in sections:
        if s['rawptr'] <= o + 1 < s['rawptr'] + s['rawsize']:
            return image_base + s['vaddr'] + (o + 1 - s['rawptr'])
    return None

def funcs_referencing(text):
    va = string_va(text)
    return sorted({func_range(site)[0] for site, tgt in zip(lea_sites, lea_targets)
                   if tgt == va and func_range(site)}) if va else []

def calls_in(lo, hi):
    """rel32 call and jmp targets (tail calls included), thunks followed. A raw scan
    can hit operand bytes; callers only compare against known function addresses,
    which makes that harmless."""
    o = va_to_off(lo)
    blob = data[o:o + hi - lo]
    for opcode in (b'\xe8', b'\xe9'):
        p = blob.find(opcode)
        while p != -1:
            if p + 5 <= len(blob):
                rel, = struct.unpack_from('<i', blob, p + 1)
                yield resolve_thunks(lo + p + 5 + rel)
            p = blob.find(opcode, p + 1)

def catch_funclets(va):
    """Catch-handler funclet addresses of a function with an FH3 try map (`.pdata`
    unwind info -> handler data -> tryblock/catchblock tables)."""
    rng = func_range(va)
    if not rng: return []
    uo = rva_to_off(next(u for b, _, u in unwind if b == va - image_base))
    flags, cnt = data[uo] >> 3, data[uo + 2]
    if not flags & 3 or flags & 4: return []
    handler_data, = struct.unpack_from('<I', data, uo + 4 + 2 * ((cnt + 1) & ~1) + 4)
    fi = rva_to_off(handler_data)
    if fi is None: return []
    magic, _, _, n_try, p_try = struct.unpack_from('<IiIiI', data, fi)
    if magic & 0x1fffffff != 0x19930522: return []
    out = []
    for t in range(n_try):
        _, _, _, n_catch, p_catch = struct.unpack_from('<iiiiI', data, rva_to_off(p_try) + 20 * t)
        for c in range(n_catch):
            _, ptd, _, handler, _ = struct.unpack_from('<IIiII', data, rva_to_off(p_catch) + 20 * c)
            out.append((handler and image_base + handler, type_descriptor_name(ptd) if ptd else '...'))
    return out

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

STUB_TEXT = "Specified method is not implemented on current facade."
LOG_NAME = re.compile(r'^([A-Z][A-Za-z0-9]+)(?:: (.*))?$')

# message string -> (error class, what failed). The helpers are located by the
# string they format, so the addresses follow the build instead of being pinned.
PRECONDITIONS = [
    ("%s: not logged on", 'UnauthorizedAccess', 'not logged on'),
    ("%s: not signed in", 'UnauthorizedAccess', 'not signed in'),
    ("%s: not signed in with Galaxy", 'InvalidState', 'not signed in with Galaxy'),
    ("%s: not signed in with GOG", 'InvalidState', 'not signed in with GOG'),
    ("%s: already signing in", 'InvalidState', 'already signing in'),
    ("%s: method not available on this platform", 'Runtime', 'not available on this platform'),
    ("'%s' is null", 'InvalidArgument', 'argument null'),
    ("'%s' is empty", 'InvalidArgument', 'argument empty'),
    ("'%s' has size of %llu bytes, which is more than the max size of %llu bytes", 'InvalidArgument', 'argument too large'),
]
ERROR_TYPE_CODES = {0: 'UnauthorizedAccess', 1: 'InvalidArgument', 2: 'InvalidState', 3: 'Runtime'}

helper_effects = defaultdict(set)
for text, cls, what in PRECONDITIONS:
    for f in funcs_referencing(text):
        helper_effects[f].add((cls, what))
# A helper that calls another helper (the Galaxy/GOG variants call "not signed in";
# the null+empty checks call the null and the empty one) fails with both. Only small
# functions propagate, so a method that merely calls a helper is not itself one.
for _ in range(3):
    for b, e, _unwind_info in unwind:
        f = image_base + b
        if e - b > 0x100 or not in_text(f): continue
        for t in calls_in(f, image_base + e):
            if t in helper_effects and t != f:
                helper_effects[f] |= helper_effects[t]

stub_text_va = string_va(STUB_TEXT)
PURECALL_DECODE = bytes.fromhex('83e13f48d3cb')

def catch_log_strings(lo):
    out = set()
    for handler, _ in catch_funclets(lo):
        if not handler: continue
        flo, fhi = func_range(handler) or (handler, handler + 0x100)
        for _, tgt in refs_in(flo, fhi):
            s = cstr(tgt, 400)
            if s and LOG_NAME.match(s) and 'facade' not in s: out.add(s)
    return sorted(out)

def classify_slot(i, real):
    o = va_to_off(real)
    head = data[o:o + 12]
    if head[:3] == b'\x48\x83\xe9' and head[4] == 0xe9: return 'thunk'
    if head[:3] == b'\x48\x81\xe9' and head[7] == 0xe9: return 'thunk'
    lo, hi = func_range(real) or (real, real + 0x100)
    if PURECALL_DECODE in data[o:o + min(hi - lo, 0x60)] and not any(
            IDENT.match(cstr(t, 80) or '') for _, t in refs_in(lo, hi)): return 'purecall'
    if stub_text_va and any(t == stub_text_va for _, t in refs_in(lo, hi)): return 'stub'
    if i == 0: return 'destructor'
    return 'method'

class Slot:
    def __init__(self, i, real):
        self.i, self.real = i, real
        self.cls = classify_slot(i, real)
        self.name = self.basis = self.args = self.note = None
        self.errors = []
        self.inferred = False
        if self.cls != 'method': return
        lo, hi = func_range(real) or (real, real + 0x100)
        logs = catch_log_strings(real)
        m = LOG_NAME.match(logs[0]) if logs else None
        self.log_name, self.args = (m.group(1), m.group(2)) if m else (None, None)
        self.body_name = method_name_at(real)
        if self.log_name: self.name, self.basis = self.log_name, 'catch-handler log'
        elif self.body_name: self.name, self.basis = self.body_name, 'string passed to a precondition helper'
        effects = []
        for t in calls_in(lo, hi):
            effects.extend(sorted(helper_effects.get(t, ())))
        for text, cls, what in PRECONDITIONS:
            va = string_va(text)
            if any(t == va for _, t in refs_in(lo, hi)): effects.append((cls, what))
        self.errors = list(dict.fromkeys(effects))
        body = data[va_to_off(real):va_to_off(real) + 8]
        if body[0] == 0xb8 and body[5] == 0xc3 and self.name is None:
            code, = struct.unpack_from('<I', body, 1)
            self.note = f"returns constant {code}" + (f" ({ERROR_TYPE_CODES[code]})" if code in ERROR_TYPE_CODES else "")

vtable_slots = OrderedDict()
for va, vt, iface, impl in ifaces:
    vtable_slots[va] = [Slot(i, resolve_thunks(s)) for i, s in enumerate(vt['slots'])]

# GOG copy-pasted a few catch-handler log strings; two slots of one vtable then log
# the same name. The body's own string settles it when it differs, and a `buffer`
# argument marks the Copy variant of the previous slot otherwise.
for va, slots in vtable_slots.items():
    seen = {}
    for sl in slots:
        if sl.cls != 'method' or not sl.name: continue
        if sl.name not in seen:
            seen[sl.name] = sl
            continue
        if sl.body_name and sl.body_name != sl.name:
            sl.name, sl.basis = sl.body_name, 'string passed to a precondition helper (the log string is a copy-paste)'
        elif sl.args and 'buffer' in sl.args:
            sl.name += 'Copy'
            sl.inferred, sl.basis = True, 'log string is a copy-paste of the previous slot; `buffer` argument marks the Copy variant'
        else:
            sl.name += 'Copy'
            sl.inferred, sl.basis = True, 'log string is a copy-paste of the previous slot; assumed Copy variant'

# stubs and _purecall carry no name; the same slot of a sibling vtable for the same interface does
names_by_slot = defaultdict(set)
for (va, vt, iface, impl) in ifaces:
    for sl in vtable_slots[va]:
        if sl.cls == 'method' and sl.name: names_by_slot[(iface, sl.i)].add(sl.name)
for (va, vt, iface, impl) in ifaces:
    for sl in vtable_slots[va]:
        if sl.cls in ('stub', 'purecall') and names_by_slot.get((iface, sl.i)):
            sl.name = '/'.join(sorted(names_by_slot[(iface, sl.i)]))
            sl.inferred, sl.basis = True, 'same slot of a sibling vtable'
        if sl.cls == 'stub': sl.errors = [('InvalidState', 'not implemented on current facade')]

n_methods = sum(1 for sls in vtable_slots.values() for sl in sls if sl.cls == 'method')
n_basis = defaultdict(int)
for sls in vtable_slots.values():
    for sl in sls:
        if sl.cls == 'method' and not sl.inferred: n_basis[sl.basis] += 1
L = []
w = L.append
w("# `Galaxy64.dll` ABI map")
w("")
w("Derived from the binary rather than from headers: GOG does not publish")
w("the Galaxy SDK without a registered developer account. Everything below")
w("comes from parsing the PE, walking its RTTI, and reading the logging")
w("strings each method passes carrying its own name.")
w("")
w("Regenerate with `python3 -I tools/gen_abi_doc.py <path-to-Galaxy64.dll> docs/galaxy64-abi.md`.")
w("The binary itself is **not** in this repo (15 MB, proprietary to GOG): copy it out")
w("of an installed game. The build this file was generated from is identified by")
w("its SHA-256 below; a different build gives different addresses, and the generator")
w("finds every helper by the strings it references rather than by address.")
w("[`galaxy64-api-notes.md`](galaxy64-api-notes.md) is a hand-written companion (argument types, return")
w("values, preconditions of the main facades); it is **not** regenerated.")
w("")
w("## What is proven and what is inferred")
w("")
w("Everything is read from the binary, in three tiers:")
w("")
w("- **Proven from the binary.** The interface of each vtable (RTTI), the slot")
w("  class (see below), and a method name read from the code of that very slot.")
w("  Every implemented facade method has ten catch handlers; one of them logs")
w("  `<Method>: arg=%fmt, ...`, which gives the method name, the argument names")
w(f"  and order, and each argument's printf type. That log string names {n_basis['catch-handler log']} of the")
w(f"  {n_methods} implemented slots; {n_basis['string passed to a precondition helper']} more take the string the body passes to a")
w("  precondition helper, which is the method name too. The *Errors* column comes")
w("  from the precondition helpers a body calls, found by their message strings")
w("  (`%s: not logged on`, `'%s' is null`, ...).")
w("- **Proven, with a caveat.** GOG copy-pasted some catch-handler log strings, so")
w("  two slots of one vtable log the same name. When the body's own string differs")
w(f"  it settles the slot ({n_basis['string passed to a precondition helper (the log string is a copy-paste)']} slot here, with a `Basis` that says so).")
w("- **Inferred**, marked with `†` and a `Basis` saying why: a stub or `_purecall`")
w("  slot has no code to read a name from, so it takes the name of the same slot in")
w("  a sibling vtable for the same interface; a copy-pasted log string with no")
w("  distinguishing body string is assumed to be the `Copy` variant when it has a")
w("  `buffer` argument (the `†` rows of implemented slots).")
w("")
w("Argument types are not decoded: the *Arguments* column is the logged text")
w("verbatim, so `%llu` is a 64-bit value and `%s` a C string, and trailing listener or")
w("out parameters that the log omits are not listed.")
w("")
w("Slot classes: `thunk` (destructor adjustor thunk, `sub rcx, N; jmp`, slot 0 of")
w("facade vtables), `destructor` (plain scalar deleting destructor), `purecall`")
w("(resolves to the CRT function that decodes the purecall handler and calls it),")
w("`stub` (references \"Specified method is not implemented on current facade.\"")
w("and stores an InvalidState error at TLS+0x110 without throwing), `method`")
w("(implemented). The implemented slots left unnamed are the four error `GetType`")
w("functions (they return a constant) and two listener-registrar methods.")
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
w(f"| SHA-256 | `{hashlib.sha256(data).hexdigest()}` |")
w(f"| `.text` virtual size | `{hex(TEXT['vsize'])}` |")
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
w("Slot 0 is always the virtual destructor: an *adjustor thunk*")
w("(`sub rcx, <offset>; jmp ...`) whenever the interface does not sit at")
w("offset 0 of the complete object, a plain destructor for the error classes.")
w("`_purecall` slots are the pure virtuals of the interface that the concrete class")
w("does not override. A `stub` is a method a facade deliberately does not implement")
w("(the game-server facades reject most peer calls this way). A name with `†` is")
w("inferred, see above. The *Errors* column lists the error classes the body can")
w("raise through precondition helpers; a blank cell means none were found.")
w("")
w("Addresses are the real function bodies, with incremental-link `jmp`")
w("thunks already followed through.")
w("")
for va, vt, iface, impl in sorted(ifaces, key=lambda x: x[2]):
    slots = vtable_slots[va]
    named = sum(1 for sl in slots if sl.name and sl.cls == 'method')
    counts = ", ".join(f"{n} {c}" for c, n in sorted({c: sum(1 for sl in slots if sl.cls == c) for c in
                                                       {sl.cls for sl in slots}}.items()))
    w(f"### `{iface}` — `{impl}`")
    w("")
    w(f"vtable `{hex(va)}`, {len(slots)} slots ({counts}); {named} implemented slots named.")
    w("")
    w("| Slot | Function | Class | Method | Arguments | Errors | Basis |")
    w("|---|---|---|---|---|---|---|")
    for sl in slots:
        name = f"`{sl.name}`" + ("†" if sl.inferred else "") if sl.name else (f"*{sl.note}*" if sl.note else "")
        args = f"`{sl.args}`" if sl.args else ""
        by_class = defaultdict(list)
        for c, what in sl.errors: by_class[c].append(what)
        errors = "; ".join(f"{c} ({', '.join(sorted(w_))})" for c, w_ in by_class.items())
        basis = sl.basis or ""
        w(f"| {sl.i} | `{hex(sl.real)}` | {sl.cls} | {name} | {args} | {errors} | {basis} |")
    w("")

open(OUT, 'w').write("\n".join(L) + "\n")
print(f"wrote {OUT}: {len(L)} lines, {len(ifaces)} interfaces")
