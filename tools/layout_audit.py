#!/usr/bin/env python3
"""Checks the layout a running Arakun really drew.

Start a build with GPUI_LAYOUT_AUDIT=<file> set (our GPUI fork then writes every
painted box, its corners, border, padding and parent to <file> each frame), open
the screens to check, and run:

    tools/layout_audit.py <file> [<file>...]

It reports, by the source line that built each box:
- radius: a box near a rounded corner of its container whose own corner is not
  the container's minus the gap between them (use ui::nested);
- inset: a chip in a bar sitting farther from the side than from the top
  (use ui::BAR_INSET / ICON_INSET);
- clip: a child as tall as a bordered parent, so the border cuts its edge.
It exits 1 when it finds a problem, and 2 when a file is not a whole dump with
drawn boxes in it, so a broken run never passes as a clean one.

GPUI rewrites <file> in place every frame, so a plain copy can catch an old
frame or half of a new one. Take each screen with

    tools/layout_audit.py snap <file> <copy> --newer-than <mark> [--differs-from <prev>]

which waits for a frame written after <mark> was touched (and unlike <prev>),
that parses whole, and copies exactly the bytes it checked.
`tools/layout-audit.sh` opens the main screens in a test Arakun and runs this.
`src/design_lint.rs` tests it against the dumps in tools/layout-audit-fixtures/.
"""
import json,os,sys,time,collections

def bad_box(r,n):
    """What is missing or wrong in one box of an n-box dump, if anything."""
    if not isinstance(r,dict): return "not an object"
    num=lambda x: isinstance(x,(int,float)) and not isinstance(x,bool)
    for k in ('b','r','border'):
        v=r.get(k)
        if not (isinstance(v,list) and len(v)==4 and all(map(num,v))): return f"{k} is not 4 numbers"
    p=r.get('parent')
    if not (isinstance(p,int) and not isinstance(p,bool) and -1<=p<n): return "parent is not -1 or a box"
    if not isinstance(r.get('bg'),bool): return "bg is not true or false"
    if not isinstance(r.get('at'),str): return "at is not a source line"
    return None

def load(buf,name):
    """The dump's boxes, or why it can't be checked."""
    try: d=json.loads(buf)
    except ValueError as e: return None,f"{name} is not whole JSON ({e}); GPUI may have been writing it"
    if not isinstance(d,list): return None,f"{name} is not a list of boxes"
    for i,r in enumerate(d):
        why=bad_box(r,len(d))
        if why: return None,f"{name}: box {i} is not a layout box ({why})"
    # Parents must lead to the root (-1); a loop would hang the walk up them.
    state=[0]*len(d)  # 0 unseen, 1 on the path being walked, 2 leads to -1
    for i in range(len(d)):
        path=[]; p=i
        while p!=-1 and state[p]==0:
            state[p]=1; path.append(p); p=d[p]['parent']
        if p!=-1 and state[p]==1:
            return None,f"{name}: box {p} is its own ancestor; the dump is not from GPUI_LAYOUT_AUDIT"
        for q in path: state[q]=2
    if not any(r['bg'] or sum(1 for x in r['border'] if x>0)>=3 for r in d):
        return None,f"{name} has no drawn boxes; the screen was empty or the dump is not from GPUI_LAYOUT_AUDIT"
    return d,None

def snap(argv):
    import argparse
    a=argparse.ArgumentParser(prog='layout_audit.py snap')
    a.add_argument('frame'); a.add_argument('copy')
    a.add_argument('--newer-than',required=True,metavar='MARK')
    a.add_argument('--differs-from',metavar='PREV')
    a.add_argument('--timeout',type=float,default=5)
    o=a.parse_args(argv)
    mark=os.stat(o.newer_than).st_mtime_ns
    prev=open(o.differs_from,'rb').read() if o.differs_from else None
    why=f"{o.frame} was never written"
    end=time.monotonic()+o.timeout
    while True:
        try:
            st=os.stat(o.frame)
            buf=open(o.frame,'rb').read()
        except OSError: buf=None
        if buf is not None:
            # Strictly newer: the mark and a frame from the same clock tick
            # can share an mtime, and that frame may predate the action.
            if st.st_mtime_ns<=mark: why=f"{o.frame} is older than this step (no frame drawn since)"
            elif buf==prev: why=f"{o.frame} still shows the same boxes as {o.differs_from} (the step changed nothing on screen)"
            else:
                _,why=load(buf,o.frame)
                if why is None:
                    open(o.copy,'wb').write(buf)
                    return 0
        if time.monotonic()>=end:
            print(f"layout audit: {why}",file=sys.stderr)
            return 2
        time.sleep(0.05)

if len(sys.argv)>1 and sys.argv[1]=='snap': sys.exit(snap(sys.argv[2:]))

FILES=sys.argv[1:]
if not FILES:
    print("usage: layout_audit.py <dump> [<dump>...]",file=sys.stderr); sys.exit(2)
out=collections.OrderedDict()
for _f in FILES:
    try: d,why=load(open(_f,'rb').read(),_f)
    except OSError as e: d,why=None,f"can't read {_f}: {e.strerror}"
    if why: print(f"layout audit: {why}",file=sys.stderr); sys.exit(2)
    def vis(r): return r['bg'] or sum(1 for x in r['border'] if x>0)>=3
    def anc(i,rounded):
        """The nearest drawn box around i (with a corner, if rounded)."""
        p=d[i]['parent']
        while p!=-1:
            if vis(d[p]) and (not rounded or max(d[p]['r'])>0): return p
            p=d[p]['parent']
        return None
    # a pill parent clips its stripes; circles and pills inside need no corner match.
    # All four corners: one rounded corner on a bar (a tab) is no pill.
    def pill(P): return min(P['r'])>=min(P['b'][2],P['b'][3])/2-0.5
    def add(at,kind,msg):
        out.setdefault((at,kind),msg)
    def corner(c,P):
        x,y,w,h=c['b']; px_,py,pw,ph=P['b']; R=P['r']
        # corners: tl,tr,br,bl distances
        ins=[(x-px_,y-py),((px_+pw)-(x+w),y-py),((px_+pw)-(x+w),(py+ph)-(y+h)),(x-px_,(py+ph)-(y+h))]
        for k in range(4):
            Rk=R[k]; dx,dy=ins[k]; cr=c['r'][k]
            if Rk<=0 or dx<0 or dy<0: continue
            if dx>=Rk or dy>=Rk: continue  # not near this corner
            if cr>=min(w,h)/2-0.5: continue  # pill / circle
            dd=(dx+dy)/2
            exp=max(Rk-dd,0)
            if abs(cr-exp)>2:
                add(c['at'],'radius',f"corner r={cr:g}, {dd:g}px inside a r={Rk:g} corner ({P['at']}): want ≈{exp:g}")
                break
    def inset(c,P):
        x,y,w,h=c['b']; px_,py,pw,ph=P['b']
        # inset: chip-like child near the left edge of a bar, vertically
        # centered; round icon buttons and pills too (only corners skip them)
        top=y-py; bot=(py+ph)-(y+h); left=x-px_
        if ph<=64 and abs(top-bot)<=1.5 and 0<=left<20 and top>0 and abs(left-top)>2:
            add(c['at'],'inset',f"{left:g}px from the side but {top:g}px from top/bottom of {P['at']}")
    for i,c in enumerate(d):
        if not vis(c): continue
        x,y,w,h=c['b']
        if w<4 or h<4: continue
        p=anc(i,True)
        if p is not None and not pill(d[p]):
            corner(c,d[p])
        # A bar can be square: insets are measured in the nearest drawn box.
        p=anc(i,False)
        if p is not None and not pill(d[p]):
            inset(c,d[p])
    # clip: child as tall as a bordered parent
    for i,c in enumerate(d):
        p=c['parent']
        if p==-1: continue
        P=d[p]; bt,_,bb,_=P['border']
        if bt+bb<=0: continue
        x,y,w,h=c['b']; px_,py,pw,ph=P['b']
        if h>=ph-0.5 and h>8 and ((y+h)>(py+ph-bb+0.5) or y<(py+bt-0.5)):
            add(c['at'],'clip',f"h={h:g} fills a {ph:g}px parent with a border ({P['at']}): its edge is cut")
for (at,kind),msg in out.items(): print(f"{at} [{kind}] {msg}")
print(f"{len(out)} problems",file=sys.stderr)
sys.exit(1 if out else 0)
