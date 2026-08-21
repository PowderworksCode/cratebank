#!/usr/bin/env python3
"""Attribute a sampled profile to per-unit compiler phases. PROTOTYPE.

    samply record --save-only --unstable-presymbolicate --include-args \
        -o prof.json --rate 999 -- cargo build
    python3 tools/phase-sample.py prof.json prof.syms.json

Nothing wraps rustc: `--include-args` puts the full rustc command line in the
profile's processName, so a unit's identity is already there. Samples carry a
pid and every compilation unit is its own process, so attribution is exact no
matter how many run concurrently -- a time-window join would be hopeless on
-j12.

Validated against nightly `-Ztime-passes` on bun_css at -Ccodegen-units=1:
every phase within ~1 point except codegen (+4.4) and macro expansion (-2.0).
At the default 16 CGUs the same crate is off by up to 10 points, because
rustc codegens on a thread per CGU: sampling measures CPU, -Ztime-passes
measures wall-clock spans. Split serial from parallel threads before comparing
against anything wall-clock; the per-CGU threads are 99.2% codegen.

Two attribution modes on the same data, both meaningful:
  outermost marker -> the enclosing phase, comparable to -Ztime-passes
  innermost marker -> which subsystem the CPU was actually in

Known gaps: ~6-10% of samples match no marker; proc-macro expansion partly
runs in the macro's own code rather than rustc_expand, so it under-counts.
"""
import json, bisect, collections, shlex, sys

# Innermost match wins, mirroring how -Ztime-passes' spans nest. Order matters
# only for ties at the same frame; the walk is leaf->root so depth decides.
MARK = [
    ("macro_expand",      ("rustc_expand::",)),
    ("resolve",           ("rustc_resolve::",)),
    ("type_check",        ("rustc_hir_typeck::",)),
    ("coherence",         ("rustc_hir_analysis::coherence",)),
    ("hir_analysis",      ("rustc_hir_analysis::",)),
    ("borrowck",          ("rustc_borrowck::",)),
    ("mir_transform",     ("rustc_mir_transform::",)),
    ("monomorphize",      ("rustc_monomorphize::",)),
    ("metadata_encode",   ("rustc_metadata::rmeta::encoder", "encode_metadata")),
    ("metadata_decode",   ("rustc_metadata::",)),
    ("llvm_codegen",      ("rustc_codegen_llvm::", "rustc_codegen_ssa::")),
    ("trait_solving",     ("rustc_trait_selection::", "rustc_next_trait_solver::")),
    ("query_overhead",    ("rustc_query_impl::", "rustc_query_system::")),
]

def load_syms(p):
    d=json.load(open(p)); st=d['string_table']; out={}
    for lib in d['data']:
        tbl=lib.get('symbol_table') or []
        out[lib['code_id']]=([e['rva'] for e in tbl],[st[e['symbol']] for e in tbl])
    return out

def unit_of(pn):
    if not pn or not pn.startswith('rustc'): return None
    try: argv=shlex.split(pn)
    except ValueError: argv=pn.split()
    name=kind=None
    for i,a in enumerate(argv):
        if a=='--crate-name' and i+1<len(argv): name=argv[i+1]
        elif a.startswith('--crate-name='): name=a.split('=',1)[1]
        elif a=='--crate-type' and i+1<len(argv): kind=kind or argv[i+1]
        elif a.startswith('--crate-type='): kind=kind or a.split('=',1)[1]
    if not name or name=='___' or any(a.startswith('--print') for a in argv): return None
    return (name, kind or 'lib')

def phase(nm):
    for lbl,pats in MARK:
        if any(p in nm for p in pats): return lbl
    return None

def run(prof_path, syms_path):
    prof=json.load(open(prof_path)); syms=load_syms(syms_path); libs=prof['libs']
    per=collections.defaultdict(collections.Counter)
    def resolve(t,fr):
        ft,fn,rt=t['frameTable'],t['funcTable'],t['resourceTable']
        res=fn['resource'][ft['func'][fr]]; addr=ft['address'][fr]
        if res is None or res<0 or rt['lib'][res] is None or addr is None or addr<0: return None
        pair=syms.get(libs[rt['lib'][res]].get('codeId'))
        if not pair: return None
        rvas,names=pair; i=bisect.bisect_right(rvas,addr)-1
        return names[i] if i>=0 else None
    for t in prof['threads']:
        u=unit_of(t.get('processName'))
        if not u: continue
        st_=t['stackTable']
        for stack in t['samples']['stack']:
            if stack is None: continue
            cur=stack; hit=None
            while cur is not None and hit is None:
                nm=resolve(t,st_['frame'][cur])
                if nm: hit=phase(nm)
                cur=st_['prefix'][cur]
            per[u][hit or "unattributed"]+=1
    return per

if __name__=='__main__':
    per=run(sys.argv[1], sys.argv[2])
    want=sys.argv[3] if len(sys.argv)>3 else None
    for u,c in sorted(per.items(), key=lambda kv:-sum(kv[1].values())):
        if want and u[0]!=want: continue
        tot=sum(c.values())
        print(f"\n{u[0]} ({u[1]}) — {tot:,} samples")
        for k,n in c.most_common():
            print(f"   {k:<20}{100*n/tot:>7.1f}%  {n:>6}")
