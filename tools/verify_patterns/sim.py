from collections import Counter
def step(c):
    n=Counter()
    for (x,y) in c:
        for dx in(-1,0,1):
            for dy in(-1,0,1):
                if dx or dy: n[(x+dx,y+dy)]+=1
    return {p for p,k in n.items() if k==3 or (k==2 and p in c)}
def art(a): return {(x,y) for y,r in enumerate(a) for x,ch in enumerate(r) if ch=='#'}
def run(c,gens,marks):
    out={}
    for g in range(1,gens+1):
        c=step(c)
        if g in marks: 
            xs=[x for x,y in c] or [0]; ys=[y for x,y in c] or [0]
            out[g]=(len(c),min(xs),max(xs),min(ys),max(ys))
    return out
