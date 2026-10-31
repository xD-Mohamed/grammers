# Longest TL nesting paths from the generated schema (read-only analysis).
import re, sys, glob
sys.setrecursionlimit(100000)
ctors = {}   # ctor name -> (result type, [field types])
types = {}   # type -> [ctors]
section = 'types'
for path in sys.argv[1:]:
    for line in open(path, encoding='utf8'):
        line = line.split('//')[0].strip()
        if not line: continue
        if line.startswith('---'):
            section = line.strip('-'); continue
        if section != 'types': continue
        m = re.match(r'^([\w.]+)#[0-9a-f]+\s+(.*?)=\s*([\w.<>]+)\s*;$', line)
        if not m: continue
        name, params, res = m.groups()
        fields = []
        for tok in params.split():
            if ':' not in tok: continue
            _, t = tok.split(':', 1)
            t = t.split('?')[-1]
            v = re.match(r'[Vv]ector<(.+)>', t)
            if v: t = v.group(1)
            fields.append(t)
        ctors[name] = (res, fields)
        types.setdefault(res, []).append(name)
PRIM = {'int','long','double','string','bytes','int128','int256','true','Bool','#','Object','!X','X','Type'}
def edges(t):
    out = set()
    for c in types.get(t, []):
        for f in ctors[c][1]:
            if f not in PRIM and f in types: out.add(f)
    return out
g = {t: edges(t) for t in types}
print('types', len(g))
# SCC
idx={};low={};st=[];on=set();sccs=[];c=[0]
def tarjan(v):
    idx[v]=low[v]=c[0];c[0]+=1;st.append(v);on.add(v)
    for w in g[v]:
        if w not in idx: tarjan(w);low[v]=min(low[v],low[w])
        elif w in on: low[v]=min(low[v],idx[w])
    if low[v]==idx[v]:
        s=[]
        while True:
            w=st.pop();on.discard(w);s.append(w)
            if w==v: break
        sccs.append(s)
for v in g:
    if v not in idx: tarjan(v)
rec=[s for s in sccs if len(s)>1 or s[0] in g[s[0]]]
print('recursive SCCs', [(len(s), sorted(s)[:40]) for s in rec])
# DAG longest path where each recursive SCC counts its member count (each member visited once).
comp={}
for i,s in enumerate(sccs):
    for v in s: comp[v]=i
memo={}
def depth(i):
    if i in memo: return memo[i]
    best=0
    for v in sccs[i]:
        for w in g[v]:
            if comp[w]!=i: best=max(best,depth(comp[w]))
    memo[i]=(len(sccs[i])+best)
    return memo[i]
for root in ['Updates','updates.Difference','updates.ChannelDifference','messages.Messages','messages.WebPage','WebPage','Page','RichText','PageBlock','help.AppConfig','JSONValue']:
    if root in comp: print(root,'upper-bound TL levels with each SCC member once',depth(comp[root]))
