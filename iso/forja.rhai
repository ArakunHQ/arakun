// Forja: a commit drops into the intake, the builder's cores light in turn,
// and three containers roll out on the rails to the edge pads, which light
// as each one lands. Then everything settles for the next push.
scene(#{
    name: "Forja",
    about: "A push goes in, the build lights up, and containers roll out to the edge.",
    seconds: 6.0,
    look: "paper",
});

let Z0 = 8.0;                      // top of the base
let BX = 110.0; let BY = 40.0;     // builder corner
let BW = 130.0; let BD = 140.0; let BH = 40.0;
let BT = Z0 + BH;                  // builder top
let CT = Z0 + 18.0;                // intake top
let n = [];

fn rails(a, b, z) {
    // two rails 4 units each side of the path a→b, on the base
    let dx = b[0] - a[0]; let dy = b[1] - a[1];
    let l = (dx * dx + dy * dy) ** 0.5;
    let nx = -dy / l * 4.0; let ny = dx / l * 4.0;
    [line([[a[0] + nx, a[1] + ny, z], [b[0] + nx, b[1] + ny, z]]).ink("mute"),
     line([[a[0] - nx, a[1] - ny, z], [b[0] - nx, b[1] - ny, z]]).ink("mute")]
}

// ---- base ---------------------------------------------------------------------
n.push(slab([0, 0, 0], [400, 220, Z0], 16));
n.push(plate(top(Z0), [10, 10, 390, 210], 10).ink("mute").dashed());
for c in [[22, 22], [378, 22], [22, 198], [378, 198]] {
    n.push(disc(top(Z0), c, 5).ink("edge"));
    n.push(disc(top(Z0), c, 2.4).fill("recess").ink("edge"));
}
// ---- intake: the commit drops into its slot ------------------------------------
n.push(slab([24, 74, Z0], [64, 72, CT - Z0], 8));
n.push(plate(top(CT), [30, 80, 82, 140], 5));
n.push(plate(top(CT), [38, 104, 74, 112], 3).fill("recess").ink("edge"));
for i in 0..4 {
    n.push(disc(top(CT), [40.0 + i * 8.0, 128], 1.4).fill("mute").ink("none"));
}
let drop = keyed(t, [[0.0, 30, "out"], [0.07, 6], [0.1, 6, "in_out"], [0.2, -50]]);
let sa = keyed(t, [[0.0, 0, "out"], [0.05, 1], [0.19, 1], [0.2, 0]]);
if sa > 0.0 {
    let z0 = CT + drop;
    n.push(slab([40, 107.4, z0], [32, 1.2, 46]).alpha(sa).layer(1).clip_above(CT));
    let lens = [20.0, 14.0, 24.0, 10.0];
    for i in 0..4 {
        let z = z0 + 38.0 - i * 6.0;
        n.push(line([[46, 108.6, z], [46 + lens[i], 108.6, z]]).alpha(sa).layer(1).clip_above(CT));
    }
    n.push(line([[46, 108.6, z0 + 8], [64, 108.6, z0 + 8]]).ink("accent").alpha(sa).layer(1).clip_above(CT));
}
// ---- cache: three disks that spin while the build reads them -----------------------
let CX = 58.0; let CY = 176.0;
n.push(line([[CX + 18, CY, Z0], [BX, CY, Z0]]).ink("accent_mute").dashed().flow(-tau * 2.0));
let busy = keyed(t, [[0.2, 0, "out"], [0.26, 1], [0.5, 1, "in_out"], [0.58, 0]]);
for i in 0..3 {
    let z = Z0 + i * 9.0;
    n.push(cylinder([CX, CY, z], 20, 7));
    n.push(disc(top(z + 7), [CX, CY], 13).ink("mute"));
    n.push(disc(top(z + 7), [CX, CY], 3).fill("recess").ink("edge"));
    let a = tau / 0.75 * 2.0 * PI() + i * 1.3;
    n.push(line([[CX + 4.0 * cos(a), CY + 4.0 * sin(a), z + 7], [CX + 12.0 * cos(a), CY + 12.0 * sin(a), z + 7]]).ink("accent").alpha(busy).layer(5 + i));
}
for i in 0..3 {
    n.push(disc(top(Z0), [CX + 24.0 + i * 6.0, CY + 14.0], 1.2).fill("mute").ink("none"));
}

// cable from the intake into the builder
n.push(line([[88, 110, Z0], [BX, 110, Z0]]).ink("accent_mute").dashed().flow(tau * 2.0));

// ---- the builder ---------------------------------------------------------------
n.push(slab([BX, BY, Z0], [BW, BD, BH], 12));
n.push(plate(top(BT), [BX + 8, BY + 8, BX + BW - 8, BY + BD - 8], 8));

// progress gauge and vents on the left wall
let prog = keyed(t, [[0.2, 0, "in_out"], [0.48, 1], [0.86, 1, "in_out"], [0.96, 0]]);
n.push(plate(left(BY + BD), [BX + 14, Z0 + 26, BX + BW - 14, Z0 + 34], 4).fill("recess").ink("edge"));
let gw = (BW - 32.0) * prog;
if gw >= 6.0 {
    n.push(plate(left(BY + BD), [BX + 16, Z0 + 28, BX + 16 + gw, Z0 + 32], 2).accent().ink("accent"));
}
for i in 0..11 {
    let x = BX + centered(11, 4, 9, BW) + i * 9.0;
    n.push(plate(left(BY + BD), [x, Z0 + 8, x + 4, Z0 + 20], 2).fill("recess").ink("mute"));
}

// fans and the exit port on the right wall
let mid = BY + BD / 2.0;
for fy in [BY + 26.0, BY + BD - 26.0] {
    n.push(disc(right(BX + BW), [fy, Z0 + 20], 12).fill("recess").ink("edge"));
    n.push(disc(right(BX + BW), [fy, Z0 + 20], 2).fill("ink").ink("none"));
    for k in 0..3 {
        let a = tau / 1.5 * 2.0 * PI() + k * 2.0 * PI() / 3.0;
        n.push(line([[BX + BW, fy, Z0 + 20], [BX + BW, fy + 10.0 * cos(a), Z0 + 20 + 10.0 * sin(a)]]).ink("mute"));
    }
}
n.push(plate(right(BX + BW), [mid - 13, Z0 + 1, mid + 13, Z0 + 22], 4).fill("recess").ink("edge"));

// eight build cores on top, lit in turn by the build
for c in 0..8 {
    let col = c % 4; let row = c / 4;
    let x0 = BX + centered(4, 20, 25, BW) + col * 25.0;
    let y0 = BY + 22.0 + row * 50.0;
    let r = [x0, y0, x0 + 20, y0 + 44];
    n.push(plate(top(BT), r, 3).fill("recess").ink("mute"));
    let on = 0.21 + c * 0.03;
    let lit = keyed(t, [[on, 0, "out"], [on + 0.05, 1], [0.84 - c * 0.012, 1, "in_out"], [0.92 - c * 0.012, 0]]);
    if lit > 0.0 {
        n.push(plate(top(BT), r, 3).accent().ink("accent").alpha(lit).layer(20 + c));
    }
}
// status light
let beat = 0.55 + 0.45 * cos(tau / 1.2 * 2.0 * PI());
n.push(disc(top(BT), [BX + BW - 18, BY + BD - 12], 3).accent().ink("none").alpha(beat).layer(30));

// ---- rails to the edge pads -----------------------------------------------------
let EXIT = [BX + BW + 8, mid];
let FORK = [300.0, mid];
let pads = [[360.0, 50.0], [372.0, mid], [360.0, 170.0]];
let paths = [];
for p in pads {
    paths.push([[EXIT[0] - 14, EXIT[1], Z0], [FORK[0], FORK[1], Z0], [p[0], p[1], Z0]]);
}
for r in rails([EXIT[0], EXIT[1]], FORK, Z0) { n.push(r); }
for p in pads {
    for r in rails(FORK, p, Z0) { n.push(r); }
}
// ties under the trunk
for i in 0..6 {
    let x = EXIT[0] + 4 + i * 9.0;
    n.push(line([[x, mid - 6, Z0], [x, mid + 6, Z0]]).ink("mute"));
}

// ---- the edge pads, back to front, each with its containers ------------------------
for k in 0..3 {
    let p = pads[k];
    let land = 0.62 + k * 0.06;
    let lit = keyed(t, [[land, 0, "out"], [land + 0.04, 1], [0.9, 1, "in_out"], [0.97, 0]]);
    // the pad: a recessed ring with a beacon at its back
    n.push(disc(top(Z0), p, 15).fill("recess").ink("edge"));
    n.push(disc(top(Z0), p, 10).ink("mute"));
    for j in 0..12 {
        let a = j * PI() / 6.0;
        n.push(line([[p[0] + 11.5 * cos(a), p[1] + 11.5 * sin(a), Z0], [p[0] + 13.5 * cos(a), p[1] + 13.5 * sin(a), Z0]]).ink("mute"));
    }
    if lit > 0.0 {
        n.push(disc(top(Z0), p, 15).ink("accent").alpha(lit).layer(50 + k));
        let ring = (tau / 1.5) % 1.0;
        n.push(disc(top(Z0), p, 15 + 10.0 * ring).ink("accent_mute").alpha(lit * (1.0 - ring)).layer(60 + k));
    }
    n.push(cylinder([p[0] - 14, p[1] - 14, Z0], 4, 26));
    n.push(disc(top(Z0 + 26), [p[0] - 14, p[1] - 14], 4).accent().ink("accent").alpha(lit).layer(70 + k));

    // its container: out of the port, along the rails, onto the pad
    let s = 0.46 + k * 0.06;
    let u = ease("in_out", progress(t, s, land));
    let ca = keyed(t, [[s - 0.02, 0, "out"], [s, 1], [0.9, 1], [0.97, 0]]);
    if ca > 0.0 {
        let q = along(paths[k], u);
        let box = slab([q[0] - 7, q[1] - 7, Z0], [14, 14, 12], 3).accent().alpha(ca).layer(80 + k);
        n.push(box.clip([BX + BW, -999, -999], [999, 999, 999]));
        n.push(plate(top(Z0 + 12), [q[0] - 4, q[1] - 4, q[0] + 4, q[1] + 4], 1.5).ink("accent_mute").alpha(ca).layer(80 + k).clip([BX + BW, -999, -999], [999, 999, 999]));
    }
}

// ---- callouts ---------------------------------------------------------------------
n.push(label([32, 82, CT], -40, "GIT PUSH", 9));
n.push(label([pads[2][0] + 15, pads[2][1], Z0], 60, "312 REGIONES", 9));

n
