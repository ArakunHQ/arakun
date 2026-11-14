// Forja metrics: traffic per region rises and falls on the bars while the
// latency dial breathes; the region you are watching is the lit bar.
scene(#{
    name: "Forja · métricas",
    about: "Live traffic and latency for every region.",
    seconds: 5.4,
    look: "paper",
});

let Z0 = 8.0;
let PT = Z0 + 6.0;                 // top of the chart plinth
let n = [];

n.push(slab([0, 0, 0], [200, 120, Z0], 12));
n.push(plate(top(Z0), [8, 8, 192, 112], 7).ink("mute").dashed());

// the chart plinth with a grid of ticks
n.push(slab([16, 16, Z0], [128, 88, PT - Z0], 6));
n.push(plate(top(PT), [22, 22, 138, 98], 4));
// bars: 6 regions × 3 rows, back to front
let base = [16.0, 24.0, 20.0, 30.0, 18.0, 26.0];
let cycles = [2.0, 3.0, 1.0, 2.0, 4.0, 3.0];   // per loop, so it closes exactly
for row in 0..3 {
    for i in 0..6 {
        let x = 22.0 + centered(6, 10, 18, 116) + i * 18.0;
        let y = 30.0 + row * 24.0;
        let ph = i * 0.9 + row * 1.7;
        let h = (base[i] - row * 4.0) * (0.75 + 0.25 * sin(t * cycles[i] * 2.0 * PI() + ph));
        let s = slab([x, y, PT], [10, 10, h], 2);
        if row == 1 && i == 3 { s = s.accent(); }
        n.push(s);
    }
}

// the latency dial on the right
let DX = 170.0; let DY = 60.0;
n.push(cylinder([DX, DY, Z0], 18, 8));
n.push(disc(top(Z0 + 8), [DX, DY], 13).ink("mute"));
for j in 0..9 {
    let a = PI() * (0.75 + j * 0.1875);
    n.push(line([[DX + 11.0 * cos(a), DY + 11.0 * sin(a), Z0 + 8], [DX + 13.0 * cos(a), DY + 13.0 * sin(a), Z0 + 8]]).ink("mute"));
}
let na = PI() * (1.1 + 0.25 * (0.5 - 0.5 * cos(t * 2.0 * 2.0 * PI())));
n.push(line([[DX, DY, Z0 + 8], [DX + 11.0 * cos(na), DY + 11.0 * sin(na), Z0 + 8]]).ink("accent"));
n.push(disc(top(Z0 + 8), [DX, DY], 2).fill("ink").ink("none"));

// a row of status pins along the front
for i in 0..6 {
    let x = 22.0 + centered(6, 10, 18, 116) + i * 18.0 + 5.0;
    n.push(disc(top(Z0), [x, 110], 1.3).fill("mute").ink("none"));
}

n.push(label([DX + 9, DY - 9, Z0 + 8], 44, "P95 · 38 MS", 9));
n
