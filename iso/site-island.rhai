// The Island as hardware: a monitor with a black pill at the top of its
// screen. A raccoon face and three agent dots live in it; the dots pulse while
// the agents work. One agent needs you: the pill grows into a card with an
// Approve button, the cursor presses it, and the pill shrinks back.
scene(#{
    name: "Island",
    about: "Arakun's Island: every agent at a glance, and approvals right where you look.",
    seconds: 5.4,
    look: "arakun",
});

let Z0 = 8.0;
let W = 380.0; let D = 160.0;
// the monitor: an upright slab, its face toward the viewer (down-left)
let MX0 = 36.0; let MW = 300.0; let MY = 46.0; let MT = 10.0;
let ZS = Z0 + 40.0;                 // bottom of the screen slab
let MH = 156.0;
let FY = MY + MT;                   // the face plane
let CX = MX0 + MW / 2.0;            // the island's center
let TOPZ = ZS + MH - 10.0;          // the island hangs from here
let TAU2 = 2.0 * PI();
let n = [];

// ---- story clock -------------------------------------------------------------------------
let NEED = 0.2;       // an agent needs you: its dot turns accent
let O0 = 0.28; let O1 = 0.38;   // the pill opens
let PRESS = 0.6;
let C0 = 0.72; let C1 = 0.82;   // and closes
let open_ = keyed(t, [[O0, 0, "in_out"], [O1, 1], [C0, 1, "in_out"], [C1, 0]]);
let card = keyed(t, [[O1 - 0.03, 0, "out"], [O1 + 0.04, 1], [C0 - 0.06, 1, "in_out"], [C0 + 0.01, 0]]);
let small = keyed(t, [[O0, 1, "in_out"], [O0 + 0.05, 0], [C1 - 0.05, 0, "in_out"], [C1, 1]]);

// ---- desk ---------------------------------------------------------------------------------
n.push(slab([0, 0, 0], [W, D, Z0], 16));
n.push(plate(top(Z0), [10, 10, W - 10, D - 10], 10).ink("mute").dashed());
for c in [[22, 22], [W - 22, 22], [22, D - 22], [W - 22, D - 22]] {
    n.push(disc(top(Z0), c, 5).ink("edge"));
    n.push(disc(top(Z0), c, 2.4).fill("recess").ink("edge"));
}

// ---- stand: foot on the desk, a neck behind the screen -----------------------------------------
n.push(slab([CX - 50, MY - 30, Z0], [100, 62, 4], 10));
n.push(slab([CX - 18, MY - 16, Z0 + 4], [36, 10, ZS - Z0 + 40], 4));

// ---- the monitor ------------------------------------------------------------------------------
n.push(slab([MX0, MY, ZS], [MW, MT, MH], 6));
n.push(plate(left(FY), [MX0 + 7, ZS + 14, MX0 + MW - 7, ZS + MH - 7], 4).fill("top").ink("edge"));
// chin: a small logo light and vents along the bottom of the bezel
n.push(disc(left(FY), [CX, ZS + 7], 1.6).fill("mute").ink("none"));
for i in 0..18 {
    let x = MX0 + centered(18, 0, 6, MW) + i * 6.0;
    n.push(line([[x, MY, ZS - 0.01], [x, FY, ZS - 0.01]]).ink("mute"));
}
// screws on the right wall
for z in [ZS + 20, ZS + MH - 20] {
    n.push(disc(right(MX0 + MW), [MY + MT / 2.0, z], 1.4).ink("mute"));
}

// ---- the desktop on the screen: two windows and a dock -----------------------------------------
let SZ0 = ZS + 14.0;
// left window: the Arakun app, sidebar and lines
let w1 = [MX0 + 18, SZ0 + 22, MX0 + 150, SZ0 + 112];
n.push(plate(left(FY), w1, 3).fill("recess").ink("mute"));
n.push(plate(left(FY), [w1[0], w1[3] - 9, w1[2], w1[3]], 3).ink("mute"));
for i in 0..3 {
    n.push(disc(left(FY), [w1[0] + 6 + i * 5.0, w1[3] - 4.5], 1.2).fill("mute").ink("none"));
}
n.push(line([[w1[0] + 34, FY, w1[1]], [w1[0] + 34, FY, w1[3] - 9]]).ink("mute"));
for i in 0..5 {
    let z = w1[3] - 20.0 - i * 13.0;
    n.push(plate(left(FY), [w1[0] + 6, z, w1[0] + 28, z + 5], 1.5).fill("mute").ink("none").alpha(0.6).layer(2));
}
let lens = [70.0, 56.0, 80.0, 44.0, 64.0, 36.0];
for i in 0..6 {
    let z = w1[3] - 22.0 - i * 12.0;
    let w = lens[i] * (0.86 + 0.14 * sin((t * 2.0 + i * 0.17) * TAU2));
    n.push(line([[w1[0] + 42, FY, z], [w1[0] + 42 + w, FY, z]]).ink("mute"));
}
// right window: a terminal with a running bar
let w2 = [MX0 + 166, SZ0 + 28, MX0 + MW - 20, SZ0 + 100];
n.push(plate(left(FY), w2, 3).fill("recess").ink("mute"));
n.push(plate(left(FY), [w2[0], w2[3] - 9, w2[2], w2[3]], 3).ink("mute"));
for i in 0..4 {
    let z = w2[3] - 20.0 - i * 10.0;
    let ln = [60.0, 44.0, 70.0, 30.0];
    n.push(line([[w2[0] + 8, FY, z], [w2[0] + 8 + ln[i], FY, z]]).ink("mute"));
}
let run = (t * 3.0) % 1.0;
n.push(plate(left(FY), [w2[0] + 8, w2[1] + 7, w2[2] - 8, w2[1] + 11], 2).ink("mute"));
let rw = (w2[2] - w2[0] - 16.0) * run;
if rw > 4.0 {
    n.push(plate(left(FY), [w2[0] + 8, w2[1] + 7, w2[0] + 8 + rw, w2[1] + 11], 2).fill("mute").ink("none"));
}
// the dock
for i in 0..9 {
    let x = CX + centered(9, 9, 13, 0.0) + i * 13.0;
    n.push(plate(left(FY), [x, SZ0 + 5, x + 9, SZ0 + 14], 2.5).ink("mute"));
}

// ---- the island ---------------------------------------------------------------------------------
let hw = lerp(44.0, 104.0, open_);        // half width
let ph = lerp(17.0, 84.0, open_);        // height
let pr = lerp(8.5, 18.0, open_);
let pill = [CX - hw, TOPZ - ph, CX + hw, TOPZ];
// a thin black body: the back plate shows as a rim below the face
n.push(plate(left(FY + 0.1), [pill[0], pill[1] - 1.2, pill[2], pill[3]], pr).fill("right").ink("edge"));
n.push(plate(left(FY + 1.6), pill, pr).fill("right").ink("edge"));
let PF = FY + 1.7;

// collapsed: the raccoon on the left, three agent dots on the right
fn raccoon(cx, cz, s, face, a, id) {
    [disc(left(face), [cx - 3.0 * s, cz + 3.2 * s], 1.6 * s).fill("mute").ink("none").alpha(a).layer(id),
     disc(left(face), [cx + 3.0 * s, cz + 3.2 * s], 1.6 * s).fill("mute").ink("none").alpha(a).layer(id),
     disc(left(face), [cx, cz], 4.2 * s).fill("mute").ink("none").alpha(a).layer(id),
     plate(left(face), [cx - 4.0 * s, cz - 0.4 * s, cx + 4.0 * s, cz + 1.8 * s], 1.0 * s).fill("right").ink("none").alpha(a).layer(id),
     disc(left(face), [cx - 1.7 * s, cz + 0.7 * s], 0.55 * s).fill("ink").ink("none").alpha(a).layer(id),
     disc(left(face), [cx + 1.7 * s, cz + 0.7 * s], 0.55 * s).fill("ink").ink("none").alpha(a).layer(id),
     disc(left(face), [cx, cz - 2.2 * s], 0.7 * s).fill("right").ink("none").alpha(a).layer(id)]
}
let cz = TOPZ - 8.5;
if small > 0.0 {
    for s in raccoon(CX - 30, cz, 1.3, PF, small, 10) { n.push(s); }
    for i in 0..3 {
        let x = CX + 12.0 + i * 9.0;
        let ph_ = 0.5 - 0.5 * cos((t * 3.0 - i * 0.22) * TAU2);
        let need = if i == 1 { keyed(t, [[NEED, 0, "out"], [NEED + 0.04, 1], [C0, 1, "in_out"], [C0 + 0.02, 0]]) } else { 0.0 };
        n.push(disc(left(PF), [x, cz], 2.0 + 0.6 * ph_).fill("mute").ink("none").alpha(small * (0.5 + 0.5 * ph_) * (1.0 - need)).layer(11 + i));
        if need > 0.0 {
            n.push(disc(left(PF), [x, cz], 2.6).accent().ink("none").alpha(small * need).layer(15));
            n.push(disc(left(PF), [x, cz], 2.6 + 3.0 * ph_).ink("accent_mute").alpha(small * need * (1.0 - ph_)).layer(16));
        }
    }
}

// expanded: the card
if card > 0.0 {
    let L = pill[0] + 12.0; let R = pill[2] - 12.0; let T = pill[3] - 10.0;
    for s in raccoon(L + 7, T - 7, 1.6, PF, card, 20) { n.push(s); }
    // who is asking, and what
    n.push(plate(left(PF), [L + 20, T - 6, L + 80, T - 2.5], 1.5).fill("mute").ink("none").alpha(card).layer(21));
    n.push(disc(left(PF), [R - 3, T - 4], 2).accent().ink("none").alpha(card).layer(22));
    n.push(plate(left(PF), [L + 20, T - 13, L + 58, T - 10.5], 1).fill("mute").ink("none").alpha(card * 0.6).layer(23));
    n.push(plate(left(PF), [L, T - 38, R, T - 22], 3).fill("top").ink("mute").alpha(card).layer(24));
    n.push(line([[L + 6, PF, T - 30], [L + 84, PF, T - 30]]).ink("mute").alpha(card).layer(24));
    // Deny (outline) and Approve (accent) with a check
    let BZ0 = pill[1] + 10.0; let BZ1 = BZ0 + 15.0;
    let mid = (L + R) / 2.0;
    n.push(plate(left(PF), [L, BZ0, mid - 4, BZ1], 7.5).ink("mute").alpha(card).layer(25));
    n.push(line([[(L + mid) / 2.0 - 3, PF, BZ0 + 3], [(L + mid) / 2.0 + 3, PF, BZ1 - 3]]).ink("mute").alpha(card).layer(25));
    n.push(line([[(L + mid) / 2.0 - 3, PF, BZ1 - 3], [(L + mid) / 2.0 + 3, PF, BZ0 + 3]]).ink("mute").alpha(card).layer(25));
    let flash = keyed(t, [[PRESS, 0, "out"], [PRESS + 0.02, 1], [PRESS + 0.1, 0]]);
    let dip = keyed(t, [[PRESS - 0.01, 0, "out"], [PRESS + 0.01, 1], [PRESS + 0.06, 0]]);
    let ab = [mid + 4 + dip, BZ0 + dip * 0.5, R - dip, BZ1 - dip * 0.5];
    n.push(plate(left(PF), ab, 7.5).accent().ink("accent").alpha(card).layer(26));
    let ck = (ab[0] + ab[2]) / 2.0;
    n.push(line([[ck - 5, PF, BZ0 + 8], [ck - 1.5, PF, BZ0 + 4], [ck + 5, PF, BZ1 - 3.5]]).ink("edge").alpha(card).layer(26));
    if flash > 0.0 {
        n.push(plate(left(PF), [ab[0] - 3, ab[1] - 3, ab[2] + 3, ab[3] + 3], 8).ink("accent").alpha(card * flash).layer(27));
        n.push(plate(left(PF), [ab[0] - 7, ab[1] - 7, ab[2] + 7, ab[3] + 7], 11).ink("accent_mute").alpha(card * flash * 0.7).layer(28));
    }
    // the cursor glides in and taps
    let cu = ease("in_out", progress(t, O1 + 0.04, PRESS - 0.02));
    let tipx = lerp(R + 22, ck + 6, cu);
    let tipz = lerp(BZ0 - 30, BZ0 + 5, cu);
    let s = 1.0 - 0.12 * dip;
    let arrow = [[0, 0], [0, -15], [4, -11.5], [7, -18], [9.6, -17], [6.8, -10.6], [12, -10.6], [0, 0]];
    let pts = [];
    for p in arrow { pts.push([tipx + p[0] * s, PF + 0.2, tipz + p[1] * s]); }
    let ca = card * keyed(t, [[O1 + 0.04, 0, "out"], [O1 + 0.08, 1], [C0 - 0.08, 1, "in_out"], [C0 - 0.03, 0]]);
    n.push(line(pts).ink("edge").alpha(ca).layer(30));
}

// ---- on the desk: keyboard and a mug -------------------------------------------------------------
let KX = CX - 92.0; let KY = 96.0;
n.push(slab([KX, KY, Z0], [184, 50, 5], 5));
for r in 0..4 {
    for c in 0..14 {
        let x = KX + centered(14, 10, 12.4, 184) + c * 12.4;
        let y = KY + centered(4, 9, 11.5, 50) + r * 11.5;
        n.push(plate(top(Z0 + 5), [x, y, x + 10, y + 9], 1.5).ink("mute"));
    }
}
// mug: a tube with a handle, steam rising
let GX = MX0 + MW - 6.0; let GY = 118.0;
n.push(tube_back([GX, GY, Z0], 11, 9, 20));
n.push(disc(top(Z0 + 16), [GX, GY], 9).fill("recess").ink("none"));
n.push(tube_front([GX, GY, Z0], 11, 9, 20));
n.push(line([[GX + 10.5, GY, Z0 + 15], [GX + 15, GY, Z0 + 15], [GX + 17, GY, Z0 + 13], [GX + 17, GY, Z0 + 8], [GX + 15, GY, Z0 + 6], [GX + 10.5, GY, Z0 + 6]]).ink("edge"));
for k in 0..2 {
    let ph_ = (t * 2.0 + k * 0.5) % 1.0;
    let a = keyed(ph_, [[0, 0, "out"], [0.3, 0.7], [1, 0]]);
    let pts = [];
    for i in 0..8 {
        let z = Z0 + 24 + ph_ * 10.0 + i * 2.5;
        pts.push([GX - 3 + k * 6.0 + 1.8 * sin((i * 0.25 + ph_) * TAU2), GY, z]);
    }
    n.push(line(pts).ink("mute").alpha(a).layer(40 + k));
}

// ---- callout ------------------------------------------------------------------------------------
n.push(label([CX, FY + 1.6, TOPZ + 1], 150, "YOUR AGENTS, AT A GLANCE", 9));

n
