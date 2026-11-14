// CPU: the heat spreader lifts off, work runs in along the bus, and the
// cores light up one by one; then they go idle and the spreader settles.
scene(#{
    name: "CPU",
    about: "The heat spreader lifts, work arrives on the bus, and the cores light up one by one.",
    seconds: 5.0,
    look: "paper",
});

let Z0 = 5.0;          // top of the board
let n = [];

// ---- board -------------------------------------------------------------------
n.push(slab([0, 0, 0], [260, 200, Z0], 12));

// Mounting holes in the corners.
for c in [[14, 14], [246, 14], [14, 186], [246, 186]] {
    n.push(disc(top(Z0), c, 6).ink("edge"));
    n.push(disc(top(Z0), c, 3).fill("recess").ink("edge"));
}

// ---- the bus: three lanes that jog 45° and run into the socket ----------------
let lanes = [];
for k in 0..3 {
    let x = 104.0 + k * 12.0;
    lanes.push([[x - 24, 199, Z0], [x - 24, 192, Z0], [x, 168, Z0], [x, 160, Z0]]);
}
for lane in lanes {
    n.push(line(lane).ink("mute"));
}
// Edge fingers where the bus leaves the board.
for k in 0..3 {
    let x = 80.0 + k * 12.0;
    n.push(plate(left(200), [x - 3, 1, x + 3, 4]).fill("recess").ink("mute"));
}

// ---- small parts ----------------------------------------------------------------
// A row of resistors, each wired to the socket by a trace that ends in a pad.
for i in 0..6 {
    let y = 62.0 + i * 16.0;
    n.push(line([[30, y + 2, Z0], [46, y + 2, Z0], [54, y - 6, Z0], [68, y - 6, Z0]]).ink("mute"));
    n.push(disc(top(Z0), [30, y + 2], 1.6).fill("mute").ink("none"));
    n.push(slab([36, y, Z0], [7, 4, 1.6], 0.6));
}

// Capacitors along the front right.
for c in [[214, 44], [232, 44], [223, 62]] {
    n.push(cylinder([c[0], c[1], Z0], 6, 10));
    n.push(disc(top(Z0 + 10), c, 3.5));
}

// ---- the socket, the package and the cores ------------------------------------------
let S = 6.0;           // socket height
n.push(slab([70, 30, Z0], [126, 130, S], 4));
n.push(plate(top(Z0 + S), [75, 35, 191, 155], 3));
// Pin field: a ring of dots between the socket's frame and the package.
let P = Z0 + S;
let i = 0;
while i <= 26 {
    let a = 80.0 + i * 4.0;
    for p in [[a, 40], [a, 150], [80, 40 + i * 4.0], [184, 40 + i * 4.0]] {
        n.push(disc(top(P), p, 0.75).fill("mute").ink("none"));
    }
    i += 1;
}
// Pins down the socket's right wall.
for j in 0..16 {
    let y = 40.0 + j * 7.0;
    n.push(plate(right(196), [y, Z0 + 1.5, y + 4, Z0 + 4.5]).ink("mute"));
}

let K = P + 3.0;        // top of the package
n.push(slab([86, 46, P], [94, 98, 3], 3));

// Eight cores in two rows; the work lights them in turn and they go idle.
let lit = [];
for c in 0..8 {
    let on = 0.34 + c * 0.04;
    lit.push(keyed(t, [[on, 0, "out"], [on + 0.05, 1], [0.78 - c * 0.012, 1, "in_out"], [0.86 - c * 0.012, 0]]));
}
// The work comes in at the front, so the near row lights first.
for c in 0..8 {
    let col = c % 4; let row = 1 - c / 4;
    let x0 = 96.0 + col * 19.0; let y0 = 56.0 + row * 42.0;
    let r = [x0, y0, x0 + 15, y0 + 36];
    n.push(plate(top(K), r, 2).fill("recess").ink("mute"));
    if lit[c] > 0.0 {
        n.push(plate(top(K), r, 2).accent().ink("accent").alpha(lit[c]).layer(20 + c));
    }
}

// ---- work packets running in along the lanes -----------------------------------------
for c in 0..8 {
    let on = 0.34 + c * 0.04;
    let u = progress(t, on - 0.12, on);
    if u > 0.0 && u < 1.0 {
        let p = along(lanes[c % 3], ease("in_out", u));
        let a = keyed(u, [[0, 0], [0.15, 1], [0.85, 1], [1, 0]]);
        n.push(slab([p[0] - 2, p[1] - 2, Z0], [4, 4, 1.4], 1).accent().alpha(a).layer(40 + c));
    }
}

// ---- the heat spreader: lifts, holds while the cores work, settles ---------------------
let lift = keyed(t, [[0.04, 0, "out"], [0.24, 52], [0.84, 52, "in_out"], [0.96, 0, "out"], [0.985, 1.2, "in_out"], [1.0, 0]]);
let H = K + lift;
n.push(slab([90, 50, H], [86, 90, 4], 6));
n.push(slab([98, 58, H + 4], [70, 74, 2.5], 5));
n.push(plate(top(H + 6.5), [106, 66, 160, 124], 3));

// ---- the clock: a crystal, its callout, and a heartbeat LED ------------------------
n.push(slab([212, 120, Z0], [20, 8, 4.5], 4));
n.push(label([222, 124, Z0 + 4.5], 120, "3.4 GHZ", 9));
// Traces from the crystal into the socket.
n.push(line([[212, 124, Z0], [204, 124, Z0], [196, 116, Z0]]).ink("mute"));
n.push(line([[222, 128, Z0], [222, 140, Z0], [214, 148, Z0], [200, 148, Z0]]).ink("mute"));

// A row of small capacitors along the socket's front.
for i in 0..5 {
    n.push(slab([126 + i * 11.0, 164, Z0], [5, 3, 1.6], 0.6));
}

// An 8-pin chip with its legs down the near wall, wired to the socket and the LED.
n.push(line([[158, 176, Z0], [158, 166, Z0]]).ink("mute"));
n.push(line([[168, 182, Z0], [180, 182, Z0], [192, 170, Z0], [192, 160, Z0]]).ink("mute"));
n.push(line([[150, 182, Z0], [140, 182, Z0], [132, 190, Z0]]).ink("mute"));
n.push(disc(top(Z0), [132, 190], 1.6).fill("mute").ink("none"));
n.push(slab([150, 174, Z0 + 0.5], [18, 10, 3.5], 1));
for j in 0..4 {
    let x = 152.0 + j * 4.2;
    n.push(plate(left(184), [x, Z0 - 0.4, x + 1.8, Z0 + 2.2]).fill("left").ink("edge"));
}
n.push(disc(top(Z0 + 4), [153.5, 177.5], 0.9).fill("ink").ink("none"));

// A heartbeat LED beside the chip.
let beat = keyed((t * 4.0) % 1.0, [[0, 1, "out"], [0.5, 0.25], [1, 1]]);
n.push(disc(top(Z0), [196, 188], 5.5).ink("accent_mute").alpha(beat).layer(60));
n.push(cylinder([196, 188, Z0], 2.4, 1.8).accent());

n
