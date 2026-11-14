// Branches: three agents work on their own branches beside main; each one
// drops commits on its lane as it goes. Claude's branch finishes, is marked
// ready, and its work rides a merge curve into main. Then the board clears.
scene(#{
    name: "Branches",
    about: "Every agent works on its own branch; the one that is ready merges into main.",
    seconds: 6.0,
    look: "arakun",
});

let Z0 = 8.0;
let W = 440.0; let D = 196.0;
let YM = 44.0;                         // main
let LY = [86.0, 122.0, 158.0];         // branch lanes: Claude, then two others
let FORK = [118.0, 80.0, 42.0];       // where each lane leaves main (nearest last: no crossings)
let END = 386.0;
let TAU2 = 2.0 * PI();
let n = [];

// ---- story clock ----------------------------------------------------------------------
// commits: [lane, x, time]
let commits = [
    [2, 138.0, 0.05], [0, 168.0, 0.08], [1, 158.0, 0.12], [2, 186.0, 0.16],
    [0, 204.0, 0.19], [1, 208.0, 0.24], [0, 240.0, 0.28], [2, 234.0, 0.31],
    [0, 276.0, 0.36], [1, 262.0, 0.40], [2, 288.0, 0.46],
];
let READY = 0.42;     // Claude's branch is ready
let M0 = 0.54; let M1 = 0.7;       // the merge travels
let CLEAR = 0.88;     // the board clears for the next round
let clear = keyed(t, [[CLEAR, 1, "in_out"], [0.97, 0]]);

// ---- board ----------------------------------------------------------------------------
n.push(slab([0, 0, 0], [W, D, Z0], 16));
n.push(plate(top(Z0), [10, 10, W - 10, D - 10], 10).ink("mute").dashed());
for c in [[22, 22], [W - 22, 22], [22, D - 22], [W - 22, D - 22]] {
    n.push(disc(top(Z0), c, 5).ink("edge"));
    n.push(disc(top(Z0), c, 2.4).fill("recess").ink("edge"));
}
// a time ruler down the front edge
for i in 0..36 {
    let x = 40.0 + i * 10.5;
    let h = if i % 5 == 0 { 5.0 } else { 2.5 };
    n.push(line([[x, D, Z0 - 1.5], [x, D, Z0 - 1.5 - h * 0.5]]).ink("mute"));
}

// ---- lanes: grooves in the board, a fork from main for each ------------------------------
for k in 0..3 {
    let y = LY[k]; let f = FORK[k];
    let pts = [[f, YM, Z0], [f + (y - YM) * 0.6, y, Z0], [END, y, Z0]];
    let ink = if k == 0 { "accent_mute" } else { "mute" };
    n.push(line(pts).ink(ink).dashed().flow(t * 6.0));
    n.push(plate(top(Z0), [f + (y - YM) * 0.6 + 8, y - 5, END + 6, y + 5], 5).ink("mute").alpha(0.5).layer(1 + k));
    // the lane's end stop
    n.push(slab([END + 8, y - 7, Z0], [8, 14, 6], 3));
}

// ---- main: a raised rail with its history of nodes ----------------------------------------
n.push(slab([24, YM - 4, Z0], [W - 48, 8, 3], 4));
let MZ = Z0 + 3.0;
for i in 0..19 {
    let x = 40.0 + i * 20.0;
    n.push(line([[x, YM - 4, MZ], [x, YM + 4, MZ]]).ink("mute"));
}
for x in [44.0, 88.0, 134.0, 180.0, 230.0, 288.0] {
    n.push(cylinder([x, YM, MZ], 7, 4));
    n.push(disc(top(MZ + 4), [x, YM], 3).fill("ink").ink("none"));
}

// ---- the merge curve: from Claude's last commit up to main ---------------------------------
let MX = 364.0;
let P0 = [298.0, LY[0]]; let P1 = [336.0, LY[0]]; let P2 = [334.0, YM]; let P3 = [MX, YM];
let curve = [];
for i in 0..=24 {
    let u = i / 24.0; let v = 1.0 - u;
    let x = v * v * v * P0[0] + 3.0 * v * v * u * P1[0] + 3.0 * v * u * u * P2[0] + u * u * u * P3[0];
    let y = v * v * v * P0[1] + 3.0 * v * v * u * P1[1] + 3.0 * v * u * u * P2[1] + u * u * u * P3[1];
    curve.push([x, y, Z0]);
}
let mu = ease("in_out", progress(t, M0, M1));
let drawn = keyed(t, [[M0 - 0.04, 0, "in_out"], [M0 + 0.02, 1]]);
if drawn > 0.0 && clear > 0.0 {
    n.push(line(curve).ink("accent").dashed().flow(t * 6.0).alpha(drawn * clear).layer(10));
}
// the merged node on main
let landed = keyed(t, [[M1 - 0.01, 0, "out"], [M1 + 0.03, 1]]);
n.push(cylinder([MX, YM, MZ], 7, 4).alpha(1.0 - landed * clear).layer(11));
if landed > 0.0 {
    n.push(cylinder([MX, YM, MZ], 7, 4).accent().alpha(landed * clear).layer(12));
    let r = progress(t, M1, M1 + 0.14);
    if r < 1.0 {
        n.push(disc(top(MZ), [MX, YM], 8 + 14.0 * r).ink("accent_mute").alpha(1.0 - r).layer(13));
    }
}
n.push(disc(top(MZ + 4), [MX, YM], 3).fill("ink").ink("none"));

// ---- commits, back to front by lane ---------------------------------------------------------
for k in 0..3 {
    let y = LY[k];
    for c in commits {
        if c[0] != k { continue; }
        let a = keyed(t, [[c[2], 0, "out"], [c[2] + 0.04, 1]]) * clear;
        if a <= 0.0 { continue; }
        let dz = keyed(t, [[c[2], 10, "out"], [c[2] + 0.06, 0]]);
        let id = 20 + k * 10 + (c[1] / 40.0).to_int();
        let cy = cylinder([c[1], y, Z0 + dz], 6, 5);
        let cy = if k == 0 { cy.accent() } else if k == 2 { cy.dark() } else { cy };
        n.push(cy.alpha(a).layer(id));
        n.push(disc(top(Z0 + dz + 5), [c[1], y], 2.4).fill(if k == 0 { "accent_left" } else if k == 2 { "mute" } else { "recess" }).ink(if k == 0 { "accent_mute" } else { "mute" }).alpha(a).layer(id));
    }
    // the agent at the head of its lane, pushing forward commit by commit
    let keys = [[0.0, FORK[k] + (y - YM) * 0.6 + 14.0, "in_out"]];
    for c in commits {
        if c[0] == k { keys.push([c[2] - 0.03, c[1] + 22.0, "in_out"]); }
    }
    let hx = keyed(t, keys);
    let ha = keyed(t, [[CLEAR, 1, "in_out"], [0.94, 0], [0.97, 0, "in_out"], [1.0, 1]]);
    let hx = if t > 0.94 { keys[0][1] } else { hx };
    let bob = 1.0 * (0.5 - 0.5 * cos((t * 6.0 + k * 0.33) * TAU2));
    let hz = Z0 + bob;
    n.push(slab([hx - 9, y - 9, hz], [18, 18, 13], 5).alpha(ha).layer(60 + k));
    n.push(plate(left(y + 9), [hx - 5, hz + 4, hx + 5, hz + 9], 2.5).fill("recess").ink("edge").alpha(ha).layer(60 + k));
    for ex in [hx - 2.2, hx + 2.2] {
        n.push(disc(left(y + 9), [ex, hz + 6.5], 0.9).fill("ink").ink("none").alpha(ha).layer(60 + k));
    }
    let led = if k == 0 { "accent" } else { "mute" };
    n.push(disc(top(hz + 13), [hx, y], 2).fill(led).ink("none").alpha(ha).layer(60 + k));
    if k == 0 {
        // Claude raises a flag when its branch is ready
        let fl = keyed(t, [[READY - 0.02, 0, "out"], [READY + 0.06, 1], [CLEAR, 1, "in_out"], [0.94, 0]]);
        if fl > 0.0 {
            let ph = 42.0 * fl;
            n.push(cylinder([hx + 4, y - 4, hz + 13], 0.8, ph).alpha(ha).layer(70).clip_above(hz + 13));
            if ph > 12.0 {
                n.push(plate(left(y - 4), [hx + 4.8, hz + 13 + ph - 10, hx + 16, hz + 13 + ph - 1], 1).accent().ink("accent").alpha(ha).layer(70));
            }
        }
    }
}

// ---- the merging commit: rides the curve into main --------------------------------------------
if mu > 0.0 && mu < 1.0 {
    let p = along(curve, mu);
    let a = keyed(mu, [[0, 0.6], [0.1, 1], [0.9, 1], [1, 0]]);
    n.push(cylinder([p[0], p[1], Z0 + 2], 5, 5).accent().alpha(a).layer(80));
}

// ---- callouts ---------------------------------------------------------------------------------
n.push(label([24, YM, MZ], -44, "MAIN", 9));
let rd = keyed(t, [[READY, 0, "out"], [READY + 0.05, 1]]) * clear;
let rv = keyed(t, [[READY + 0.03, 0, "out"], [READY + 0.12, 1]]);
if rd > 0.0 {
    let FL_TOP = [P0[0] + 4, LY[0] - 4, Z0 + 56];
    n.push(label(FL_TOP, 50, "CLAUDE · READY TO MERGE", 9).reveal(rv).alpha(rd).layer(90));
}

n
