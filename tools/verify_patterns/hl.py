import sys
sys.setrecursionlimit(100000)
class Node:
    __slots__=('k','a','b','c','d','pop')
    def __init__(s,k,a,b,c,d,pop): s.k=k;s.a=a;s.b=b;s.c=c;s.d=d;s.pop=pop
_cache={}
def mk(a,b,c,d):
    key=(id(a),id(b),id(c),id(d))
    n=_cache.get(key)
    if n is None:
        n=Node(a.k+1,a,b,c,d,a.pop+b.pop+c.pop+d.pop); _cache[key]=n; _keep.append((a,b,c,d))
    return n
_keep=[]
ON=Node(0,None,None,None,None,1); OFF=Node(0,None,None,None,None,0)
_e={0:OFF}
def empty(k):
    if k not in _e:
        e=empty(k-1); _e[k]=mk(e,e,e,e)
    return _e[k]
def center(n): return mk(n.a.d,n.b.c,n.c.b,n.d.a)
def leaf(n):
    g=[[0]*4 for _ in range(4)]
    for q,(ox,oy) in ((n.a,(0,0)),(n.b,(2,0)),(n.c,(0,2)),(n.d,(2,2))):
        g[oy][ox]=q.a.pop;g[oy][ox+1]=q.b.pop;g[oy+1][ox]=q.c.pop;g[oy+1][ox+1]=q.d.pop
    o=[]
    for y in(1,2):
        for x in(1,2):
            s=sum(g[y+dy][x+dx] for dy in(-1,0,1) for dx in(-1,0,1))-g[y][x]
            o.append(ON if (s==3 or (s==2 and g[y][x])) else OFF)
    return mk(*o)
_memo={}
def nine(n):
    a,b,c,d=n.a,n.b,n.c,n.d
    return [[a, mk(a.b,b.a,a.d,b.c), b],
            [mk(a.c,a.d,c.a,c.b), mk(a.d,b.c,c.b,d.a), mk(b.c,b.d,d.a,d.b)],
            [c, mk(c.b,d.a,c.d,d.c), d]]
def step(n,s):
    """level k node -> level k-1 centre advanced 2**s generations (s<=k-2)"""
    key=(id(n),s)
    r=_memo.get(key)
    if r is not None: return r
    k=n.k
    if n.pop==0: r=empty(k-1)
    elif k==2: r=leaf(n)
    else:
        N=nine(n)
        if s==k-2:
            R=[[step(N[i][j],s-1) for j in range(3)] for i in range(3)]
            qa=step(mk(R[0][0],R[0][1],R[1][0],R[1][1]),s-1)
            qb=step(mk(R[0][1],R[0][2],R[1][1],R[1][2]),s-1)
            qc=step(mk(R[1][0],R[1][1],R[2][0],R[2][1]),s-1)
            qd=step(mk(R[1][1],R[1][2],R[2][1],R[2][2]),s-1)
            r=mk(qa,qb,qc,qd)
        else:
            R=[[step(N[i][j],s) for j in range(3)] for i in range(3)]
            r=mk(center(mk(R[0][0],R[0][1],R[1][0],R[1][1])),center(mk(R[0][1],R[0][2],R[1][1],R[1][2])),
                 center(mk(R[1][0],R[1][1],R[2][0],R[2][1])),center(mk(R[1][1],R[1][2],R[2][1],R[2][2])))
    _memo[key]=r
    return r
def build(cells):
    cells=set(cells)
    xs=[x for x,y in cells];ys=[y for x,y in cells]
    w=max(max(xs)-min(xs),max(ys)-min(ys))+1
    k=2
    while (1<<k)<4*w: k+=1
    size=1<<k; ox=size//2-(max(xs)+min(xs))//2; oy=size//2-(max(ys)+min(ys))//2
    def rec(k,x0,y0):
        if k==0: return ON if (x0-ox,y0-oy) in cells else OFF
        h=1<<(k-1)
        return mk(rec(k-1,x0,y0),rec(k-1,x0+h,y0),rec(k-1,x0,y0+h),rec(k-1,x0+h,y0+h))
    # cells are in absolute coords; shift
    cells2={(x+ox,y+oy) for x,y in cells}
    def rec2(k,x0,y0):
        if k==0: return ON if (x0,y0) in cells2 else OFF
        h=1<<(k-1)
        return mk(rec2(k-1,x0,y0),rec2(k-1,x0+h,y0),rec2(k-1,x0,y0+h),rec2(k-1,x0+h,y0+h))
    return rec2(k,0,0)
def expand(n):
    e=empty(n.k-1)
    return mk(mk(e,e,e,n.a),mk(e,e,n.b,e),mk(e,n.c,e,e),mk(n.d,e,e,e))
def advance(root,s):
    while root.k<s+3: root=expand(root)
    root=expand(expand(root))   # room to grow
    return step(root,s)         # level k-1 centre
