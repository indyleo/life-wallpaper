import sys,time,random
sys.path.insert(0,'/tmp')
import hl
from classify import *
GUN=['........................#...........','......................#.#...........','............##......##............##','...........#...#....##............##','##........#.....#...##..............','##........#...#.##....#.#...........','..........#.....#.......#...........','...........#...#....................','............##......................']
GUNC=[(x,y) for y,r in enumerate(GUN) for x,ch in enumerate(r) if ch=='#']
def orient(cells,o,w,h):
    out=[]
    for x,y in cells:
        if o&1:x=w-1-x
        if o&2:y=h-1-y
        if o&4:x,y=y,x
        out.append((x,y))
    return out
def gun(o,ox,oy): return [(ox+x,oy+y) for x,y in orient(GUNC,o,36,9)]
seed=int(sys.argv[1]); budget=float(sys.argv[2]); rng=random.Random(seed)
t0=time.time(); n=0; hits=0
while time.time()-t0<budget:
    k=rng.choice([2,3,3,4]); cells=set(); desc=[]; clash=False
    for c in range(k):
        typ=rng.choice('EEGL'); o=rng.randrange(8)
        ox,oy=(0,0) if c==0 else (rng.randint(-30,30),rng.randint(-30,30))
        pts=engine(o,ox,oy) if typ=='E' else (gun(o,ox,oy) if typ=='G' else line(o&3,ox,oy))
        if cells&set(pts): clash=True
        cells|=set(pts); desc.append(f'{typ}{o}@{ox},{oy}')
    if clash: continue
    n+=1
    if len(_c:=hl._cache)>3_000_000:
        hl._cache.clear(); hl._memo.clear(); hl._keep.clear(); hl._e.clear(); hl._e[0]=hl.OFF
    try:
        p=series(cells,15,8)
    except RecursionError: continue
    if len(p)<4: continue
    r=[p[i+1][1]/max(1,p[i][1]) for i in range(len(p)-1)]
    if r[-1]>2.7 and r[-2]>2.5 and p[-1][1]>3000:
        hits+=1; print('HIT',' '.join(desc),p[-1],' '.join('%.2f'%x for x in r),flush=True)
print('tested',n,'hits',hits,flush=True)
