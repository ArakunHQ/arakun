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
`tools/layout-audit.sh` opens the main screens in a test Arakun and runs this.
"""
import json,sys,collections
FILES=sys.argv[1:]
out=collections.OrderedDict()
for _f in FILES:
    d=json.load(open(_f))
    def vis(r): return r['bg'] or sum(1 for x in r['border'] if x>0)>=3
    def anc(i):
        p=d[i]['parent']
        while p!=-1:
            if vis(d[p]) and max(d[p]['r'])>0: return p
            p=d[p]['parent']
        return None
    def add(at,kind,msg):
        out.setdefault((at,kind),msg)
    for i,c in enumerate(d):
        if not vis(c): continue
        x,y,w,h=c['b']
        if w<4 or h<4: continue
        p=anc(i)
        if p is None: continue
        P=d[p]; px_,py,pw,ph=P['b']; R=P['r']
        # a pill parent clips its stripes; circles and pills inside need no corner match
        if max(R)>=min(pw,ph)/2-0.5: continue
        round_child=max(c['r'])>=min(w,h)/2-0.5
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
        # inset: chip-like child near the left edge of a bar, vertically centered
        top=y-py; bot=(py+ph)-(y+h); left=x-px_
        if not round_child and ph<=64 and abs(top-bot)<=1.5 and 0<=left<20 and top>0 and abs(left-top)>2:
            add(c['at'],'inset',f"{left:g}px from the side but {top:g}px from top/bottom of {P['at']}")
    # clip: child as tall as a bordered parent
    for i,c in enumerate(d):
        p=c['parent']
        if p==-1: continue
        P=d[p]; bt,_,bb,_=P['border']
        if bt+bb<=0: continue
        x,y,w,h=c['b']; px_,py,pw,ph=P['b']
        if h>=ph-0.5 and h>8 and (y+h)>(py+ph-bb+0.5):
            add(c['at'],'clip',f"h={h:g} fills a {ph:g}px parent with a border ({P['at']}): its edge is cut")
for (at,kind),msg in out.items(): print(f"{at} [{kind}] {msg}")
print(f"{len(out)} problems",file=sys.stderr)
sys.exit(1 if out else 0)
