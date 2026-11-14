// Sandbox: an agent works inside a walled tray (cores light, sheets fill),
// then hands a "git push" to the gate. The gate stays shut until the approval
// lamp turns on; then it drops, the push rolls out to the remote, and the gate
// closes behind it.
scene(#{
    name: "Sandbox",
    about: "The agent works freely inside its sandbox; anything leaving it waits at the gate for your approval.",
    seconds: 5.4,
    look: "arakun",
});

let Z0 = 8.0;
let W = 420.0; let D = 230.0;
// the sandbox tray and its walls
let SX = 22.0; let SY = 22.0; let SW = 236.0; let SD = 186.0;
let TZ = Z0 + 6.0;                 // tray floor
let WH = 20.0; let WT = 4.0;       // wall height, thickness
let GY = SY + SD / 2.0;            // gate center (on the right wall)
let GW = 30.0;                     // gate width
let XR = SX + SW;                  // outside of the right wall
// the remote
let MX = 330.0; let MD = 58.0; let MY = GY - 14.0 - MD; let MW = 62.0;
let LXC = MX + MW - 18.0;         // the lane turns here into the remote's port
// the approval console in front
let QX = 316.0; let QY = 160.0; let QW = 60.0; let QD = 44.0; let QH = 10.0;
let TAU2 = 2.0 * PI();
let BIG = 9999.0;
let n = [];

// ---- story clock -------------------------------------------------------------------
let ask = 0.42;        // the push reaches the gate
let ok = 0.58;         // you approve
let open_ = keyed(t, [[ok + 0.02, 0, "in_out"], [ok + 0.08, 1], [0.84, 1, "in_out"], [0.92, 0]]);
let lamp = keyed(t, [[ok, 0, "out"], [ok + 0.03, 1], [0.86, 1, "in_out"], [0.94, 0]]);

// ---- base --------------------------------------------------------------------------
n.push(slab([0, 0, 0], [W, D, Z0], 16));
n.push(plate(top(Z0), [10, 10, W - 10, D - 10], 10).ink("mute").dashed());
for c in [[W - 22, 22], [W - 22, D - 22]] {
    n.push(disc(top(Z0), c, 5).ink("edge"));
    n.push(disc(top(Z0), c, 2.4).fill("recess").ink("edge"));
}

// ---- tray and the back walls ----------------------------------------------------------
n.push(slab([SX, SY, Z0], [SW, SD, TZ - Z0], 6));
n.push(slab([SX, SY, TZ], [SW, WT, WH], 1));
n.push(slab([SX, SY, TZ], [WT, SD, WH], 1));
// a grid on the floor
for i in 1..8 {
    let x = SX + i * SW / 8.0;
    n.push(line([[x, SY + WT, TZ], [x, SY + SD - WT, TZ]]).ink("mute").alpha(0.35).layer(1));
}
for i in 1..6 {
    let y = SY + i * SD / 6.0;
    n.push(line([[SX + WT, y, TZ], [XR - WT, y, TZ]]).ink("mute").alpha(0.35).layer(1));
}

// ---- the work: a chip whose cores light in turn ------------------------------------------
let CX = SX + 128.0; let CY = SY + 22.0; let CW = 70.0; let CD = 48.0;
n.push(slab([CX, CY, TZ], [CW, CD, 6], 4));
let CZ = TZ + 6.0;
for j in 0..10 {
    let x = CX + centered(10, 2, 6, CW) + j * 6.0;
    n.push(plate(left(CY + CD), [x, TZ + 1.5, x + 2, TZ + 4.5], 0.5).ink("mute"));
}
for c in 0..6 {
    let col = c % 3; let row = c / 3;
    let x0 = CX + centered(3, 16, 20, CW) + col * 20.0;
    let y0 = CY + centered(2, 14, 18, CD) + row * 18.0;
    let r = [x0, y0, x0 + 16, y0 + 14];
    n.push(plate(top(CZ), r, 2).fill("recess").ink("mute"));
    // two passes of work: cores flicker on in turn, then go idle
    let on = 0.04 + ((c * 7) % 6) * 0.035;
    let lit = keyed(t, [[on, 0, "out"], [on + 0.04, 1], [on + 0.12, 1, "in_out"], [on + 0.18, 0]]);
    if lit > 0.0 {
        n.push(plate(top(CZ), r, 2).accent().ink("accent").alpha(lit).layer(10 + c));
    }
}

// ---- sheets the agent writes ---------------------------------------------------------
let sheets = [[SX + 12.0, SY + 94.0], [SX + 72.0, SY + 146.0], [SX + 150.0, SY + 112.0]];
for k in 0..3 {
    let p = sheets[k];
    n.push(slab([p[0], p[1], TZ], [40, 30, 1.2]));
    let lens = [26.0, 18.0, 30.0, 14.0];
    for i in 0..4 {
        let on = 0.06 + k * 0.08 + i * 0.02;
        let w = lens[i] * keyed(t, [[on, 0, "out"], [on + 0.05, 1], [0.9, 1, "in_out"], [0.97, 0]]);
        if w >= 1.0 {
            let x = p[0] + 5.0 + i * 6.5;
            n.push(line([[x, p[1] + 4, TZ + 1.2], [x, p[1] + 4 + w * 0.8, TZ + 1.2]]).ink("mute"));
        }
    }
}

// ---- the agent: a small bot that bobs while it works ------------------------------------
let BX = SX + 66.0; let BY = SY + 66.0;
let bob = 1.6 * (0.5 - 0.5 * cos(t * 4.0 * TAU2));
let BZ = TZ + bob;
n.push(slab([BX + 3, BY + 3, TZ], [30, 28, 3], 8).fill("recess"));
n.push(slab([BX, BY, BZ + 3], [36, 34, 26], 10));
let FL = BY + 34.0;
n.push(plate(left(FL), [BX + 6, BZ + 10, BX + 30, BZ + 24], 5).fill("recess").ink("edge"));
let blink = keyed((t * 2.0) % 1.0, [[0.46, 1, "in_out"], [0.5, 0.15], [0.54, 1]]);
for ex in [BX + 13.0, BX + 23.0] {
    n.push(plate(left(FL), [ex - 2, BZ + 17 - 3.0 * blink, ex + 2, BZ + 17 + 3.0 * blink], 1.9).fill("ink").ink("none"));
}
n.push(cylinder([BX + 18, BY + 17, BZ + 29], 1.2, 8));
n.push(sphere([BX + 18, BY + 17, BZ + 39], 3));
// side vents
for i in 0..4 {
    let y = BY + centered(4, 0, 5, 34) + i * 5.0;
    n.push(line([[BX + 36, y, BZ + 11], [BX + 36, y, BZ + 21]]).ink("mute"));
}

// ---- the push: a packet from the agent to the gate, then out to the remote ------------------
let PZ = TZ;
let WAIT_X = XR - WT - 36.0;
let path = [[BX + 44, GY, PZ], [WAIT_X, GY, PZ], [LXC, GY, PZ], [LXC, MY + MD - 10, PZ]];
let L1 = WAIT_X - BX - 44.0;
let LT_ = LXC - BX - 44.0 + GY - (MY + MD - 10.0);
let pu = keyed(t, [[0.3, 0, "in_out"], [ask, L1 / LT_], [ok + 0.06, L1 / LT_, "in_out"], [ok + 0.22, 1]]);
let pa = keyed(t, [[0.28, 0, "out"], [0.32, 1], [ok + 0.19, 1], [ok + 0.22, 0]]);
let pp = along(path, pu);
let px = pp[0]; let py = pp[1];
fn push_box(x, y, z, a) {
    [slab([x - 8, y - 6, z], [16, 12, 7], 2).accent().alpha(a).layer(50),
     line([[x - 4, y + 6, z + 2.5], [x + 4, y + 6, z + 2.5]]).ink("accent_mute").alpha(a).layer(50),
     line([[x - 4, y + 6, z + 4.5], [x + 1, y + 6, z + 4.5]]).ink("accent_mute").alpha(a).layer(50)]
}
let pk = if pa > 0.0 { push_box(px, py, PZ, pa) } else { [] };
for s in pk { n.push(s.clip([-BIG, -BIG, -BIG], [XR - WT, BIG, BIG])); }

// ---- near walls: glass, so the work shows through ------------------------------------------
let GLASS = 0.5;
n.push(slab([SX, SY + SD - WT, TZ], [SW - WT, WT, WH], 1).alpha(GLASS).layer(2));
// right wall in two pieces around the gate
n.push(slab([XR - WT, SY, TZ], [WT, GY - GW / 2.0 - SY, WH], 1).alpha(GLASS).layer(3));
n.push(slab([XR - WT, GY + GW / 2.0, TZ], [WT, SY + SD - GY - GW / 2.0, WH], 1).alpha(GLASS).layer(4));
// rivets along the top of the near walls
for i in 0..11 {
    let x = SX + centered(11, 0, 21, SW) + i * 21.0;
    n.push(disc(top(TZ + WH), [x, SY + SD - WT / 2.0], 0.9).fill("mute").ink("none"));
}
for i in 0..4 {
    let y1 = SY + 12.0 + i * 16.0;
    let y2 = GY + GW / 2.0 + 12.0 + i * 16.0;
    n.push(disc(top(TZ + WH), [XR - WT / 2.0, y1], 0.9).fill("mute").ink("none"));
    n.push(disc(top(TZ + WH), [XR - WT / 2.0, y2], 0.9).fill("mute").ink("none"));
}
// the gate: a solid door that sinks into the floor when approved
let gz = TZ - (WH + 1.0) * open_;
n.push(slab([XR - WT - 1, GY - GW / 2.0, gz], [WT + 2, GW, WH + 1], 1).clip_above(TZ));
let stripes = 4;
for i in 0..stripes {
    let y = GY - GW / 2.0 + centered(stripes, 3, 7, GW) + i * 7.0;
    n.push(plate(right(XR + 1), [y, gz + 5, y + 3, gz + WH - 4], 1).fill("recess").ink("mute").clip_above(TZ));
}
// the gate's frame: two posts and a lintel
let GH = WH + 12.0;
n.push(slab([XR - WT - 2, GY - GW / 2.0 - 5, TZ], [WT + 4, 5, GH], 1));
n.push(slab([XR - WT - 2, GY - GW / 2.0 - 5, TZ + GH], [WT + 4, GW + 10, 5], 1));
n.push(slab([XR - WT - 2, GY + GW / 2.0, TZ], [WT + 4, 5, GH], 1));
n.push(plate(right(XR + 2), [GY - GW / 2.0 - 3, TZ + GH + 1.5, GY + GW / 2.0 + 3, TZ + GH + 3.5], 1).ink("mute"));


// ---- lane to the remote ---------------------------------------------------------------------
n.push(line([[XR + 2, GY, Z0], [LXC, GY, Z0], [LXC, MY + MD, Z0]]).ink("accent_mute").dashed().flow(t * 8.0).alpha(0.4 + 0.6 * open_).layer(60));
for i in 0..5 {
    let x = XR + 16.0 + i * 12.0;
    n.push(line([[x, GY - 9, Z0], [x, GY + 9, Z0]]).ink("mute"));
}

// ---- approval post with its lamp, outside the gate ------------------------------------------
let LPX = XR + 12.0; let LPY = GY - GW / 2.0 - 12.0;
n.push(slab([LPX - 6, LPY - 6, Z0], [12, 12, 4], 3));
n.push(cylinder([LPX, LPY, Z0 + 4], 2.2, 34));
n.push(cylinder([LPX, LPY, Z0 + 38], 6, 6));
let wait = if t > ask && t < ok { 0.5 - 0.5 * cos((t - ask) / 0.08 * TAU2) } else { 0.0 };
n.push(disc(top(Z0 + 44), [LPX, LPY], 4).fill("mute").ink("none").alpha(1.0 - lamp).layer(70));
if lamp > 0.0 {
    n.push(disc(top(Z0 + 44), [LPX, LPY], 4).accent().ink("none").alpha(lamp).layer(71));
    n.push(disc(top(Z0 + 44), [LPX, LPY], 9).ink("accent_mute").alpha(lamp * 0.8).layer(72));
}
if wait > 0.0 {
    n.push(disc(top(Z0 + 44), [LPX, LPY], 6 + 2.0 * wait).ink("mute").alpha(wait).layer(73));
}

// ---- the remote: a small server, three units, lights as the push lands ------------------------
let land = keyed(t, [[ok + 0.18, 0, "out"], [ok + 0.22, 1], [0.9, 1, "in_out"], [0.96, 0]]);
for u in 0..3 {
    let z = Z0 + u * 16.0;
    n.push(slab([MX, MY, z], [MW, MD, 15], 6));
    let nv = if u == 0 { 4 } else { 7 };
    for i in 0..nv {
        let x = MX + centered(7, 3, 6, MW) + i * 6.0;
        n.push(plate(left(MY + MD), [x, z + 4, x + 3, z + 11], 1).fill("recess").ink("mute"));
    }
    for i in 0..3 {
        n.push(disc(right(MX + MW), [MY + 12.0 + i * 6.0, z + 7.5], 1.3).fill("mute").ink("none"));
    }
}
n.push(plate(top(Z0 + 48), [MX + 6, MY + 6, MX + MW - 6, MY + MD - 6], 4));
n.push(plate(left(MY + MD), [LXC - 9, Z0 + 1.5, LXC + 9, Z0 + 11.5], 2).fill("recess").ink("edge"));
// the push, outside the wall: along the lane and into the port
for s in pk { n.push(s.clip([XR, MY + MD, -BIG], [BIG, BIG, BIG])); }
for u in 0..3 {
    let a = keyed(t, [[ok + 0.18 + u * 0.03, 0, "out"], [ok + 0.21 + u * 0.03, 1], [0.9, 1, "in_out"], [0.96, 0]]);
    if a > 0.0 {
        n.push(disc(right(MX + MW), [MY + 12.0, Z0 + u * 16.0 + 7.5], 1.6).accent().ink("none").alpha(a).layer(80 + u));
    }
}

// ---- the approval console: you press, the lamp turns on --------------------------------------
// its cable runs back to the lamp post
n.push(line([[QX, QY + 14, Z0], [LPX, QY + 14, Z0], [LPX, LPY + 6, Z0]]).ink("mute").dashed().flow(-t * 6.0));
n.push(slab([QX, QY, Z0], [QW, QD, QH], 7));
let QZ = Z0 + QH;
n.push(plate(top(QZ), [QX + 5, QY + 5, QX + QW - 5, QY + QD - 5], 4));
// a small screen with the request on it
n.push(plate(top(QZ), [QX + 8, QY + 8, QX + 30, QY + QD - 8], 2).fill("recess").ink("edge"));
let req = keyed(t, [[ask, 0, "out"], [ask + 0.04, 1], [0.86, 1, "in_out"], [0.94, 0]]);
for i in 0..3 {
    let ln = [18.0, 12.0, 15.0];
    let w = ln[i] * req;
    if w >= 1.5 {
        let y = QY + 11.0 + i * 0.0;
        n.push(line([[QX + 12 + i * 5.0, QY + 11, QZ], [QX + 12 + i * 5.0, QY + 11 + w, QZ]]).ink(if i == 0 { "accent" } else { "mute" }));
    }
}
// the button: rises when there is something to approve, pressed at ok
let press = keyed(t, [[ok - 0.03, 0, "in_out"], [ok, 2.5, "out"], [ok + 0.06, 0]]);
let bh = 4.0 - press;
let BCX = QX + 44.0; let BCY = QY + QD / 2.0;
n.push(disc(top(QZ), [BCX, BCY], 11).fill("recess").ink("edge"));
n.push(cylinder([BCX, BCY, QZ], 8, bh));
n.push(disc(top(QZ + bh), [BCX, BCY], 5).ink("mute"));
let btn = keyed(t, [[ask, 0, "out"], [ask + 0.04, 1], [0.86, 1, "in_out"], [0.94, 0]]);
if btn > 0.0 {
    let glow = if t < ok { 0.55 + 0.45 * cos((t - ask) / 0.08 * TAU2) } else { 1.0 };
    n.push(cylinder([BCX, BCY, QZ], 8, bh).accent().alpha(btn * glow).layer(90));
}
let ring = progress(t, ok, ok + 0.12);
if ring > 0.0 && ring < 1.0 {
    n.push(disc(top(QZ), [BCX, BCY], 11 + 10.0 * ring).ink("accent_mute").alpha(1.0 - ring).layer(91));
}
for i in 0..4 {
    n.push(disc(right(QX + QW), [QY + centered(4, 0, 6, QD) + i * 6.0, Z0 + QH / 2.0], 1.1).fill("mute").ink("none"));
}

// ---- callouts -----------------------------------------------------------------------------
n.push(label([SX + 2, SY + 40, TZ + WH], -60, "SANDBOX", 9));
n.push(label([LPX, LPY, Z0 + 50], 90, "WAITS FOR YOU", 9));

n
