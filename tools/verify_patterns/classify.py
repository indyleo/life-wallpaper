import sys,time,re
sys.path.insert(0,'/tmp')
import hl
from sim import art
E=[(6,0),(4,1),(6,1),(7,1),(4,2),(6,2),(4,3),(2,4),(0,5),(2,5)]
LINE="########.#####...###......#######.#####"
def engine(o,ox,oy):
    out=[]
    for x,y in E:
        if o&1:x=7-x
        if o&2:y=5-y
        if o&4:x,y=y,x
        out.append((ox+x,oy+y))
    return out
def line(o,ox,oy):
    out=[]
    for i,ch in enumerate(LINE):
        if ch=='#':
            x=(38-i) if o&1 else i; y=0
            if o&2:x,y=y,x
            out.append((ox+x,oy+y))
    return out
def series(cells,maxj=15,budget=60):
    t0=time.time()
    root=hl.build(cells); pops=[]; total=0
    root=hl.advance(root,10); total=1024; pops.append((total,root.pop))
    s=10
    while s<maxj and time.time()-t0<budget:
        root=hl.advance(root,s); total*=2; s+=1; pops.append((total,root.pop))
    return pops
def verdict(pops):
    if len(pops)<4: return '?'
    r=[pops[i+1][1]/max(1,pops[i][1]) for i in range(len(pops)-1)]
    return ' '.join('%.2f'%x for x in r)
if __name__=='__main__':
    from sim import art
    tests={'gun':art(['........................#...........','......................#.#...........','............##......##............##','...........#...#....##............##','##........#.....#...##..............','##........#...#.##....#.#...........','..........#.....#.......#...........','...........#...#....................','............##......................']),
           'infinite-line':{(i,0) for i,c in enumerate(LINE) if c=='#'},
           'engine':set(engine(0,0,0))}
    for k,c in tests.items():
        t=time.time(); p=series(c,13,50); print(k,p[-1],verdict(p),'%.1fs'%(time.time()-t),flush=True)
