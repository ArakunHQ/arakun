// Usage billing: one loop is a month. A coin of usage drops into the hopper
// each week, the gauge and the screen's weekly bars climb with them, then the
// month closes: the coins sink, last month's invoice lifts away and a new one
// prints out of the slot.
scene(#{
    name: "Usage billing",
    about: "Usage drops into the meter, stacks up through the month, and the meter prints the invoice.",
    seconds: 3.6,
});

let TOP = 68.0;        // top of the meter's body
let COIN_H = 4.0;
let PAPER_H = 52.0;
let HX = 70.0; let HY = 80.0;   // the hopper's center

let n = [];

// Base and body.
n.push(slab([0, 0, 0], [220, 160, 8], 16));
n.push(slab([20, 20, 8], [180, 120, TOP - 8], 10));
n.push(plate(top(TOP), [27, 27, 193, 133], 6));

// When each coin starts to fall, when it lands, and how landed it is (0..1),
// back to 0 as the month closes.
let starts = []; let lands = []; let landed = [];
for i in 0..4 {
    let start = 0.069 + 0.1 * i;
    let land = start + 0.0945;
    starts.push(start); lands.push(land);
    landed.push(keyed(t, [[land - 0.01, 0, "out"], [land + 0.06, 1], [0.5417, 1, "in_out"], [0.6667, 0]]));
}

// Gauge on the left wall: a step per coin.
let level = (landed[0] + landed[1] + landed[2] + landed[3]) / 4.0;
n.push(plate(left(140), [36, 20, 112, 56], 6).fill("recess").ink("edge"));
let fill_w = 64.0 * level;
if fill_w >= 6.0 {
    n.push(plate(left(140), [42, 30, 42 + fill_w, 42], 3).accent().ink("accent"));
}
for i in 0..5 {
    let x = 44.0 + i * 15.0;
    n.push(line([[x, 140, 46], [x, 140, 51]]));
}

// Screen on the right wall: a bar per week, raised by that week's coin.
n.push(plate(right(200), [34, 22, 104, 58], 6).fill("recess").ink("edge"));
n.push(line([[200, 96, 27], [200, 42, 27]]));
let heights = [14.0, 22.0, 17.0, 26.0];
for i in 0..4 {
    let y = 94.0 - i * 14.0;
    let h = heights[i] * landed[i];
    if h >= 4.0 {
        n.push(plate(right(200), [y - 8, 28, y, 28 + h], 2).accent().ink("accent"));
    }
}

// Print button: flashes when the invoice comes out.
let flash = keyed(t, [[0.661, 0, "out"], [0.689, 1, "out"], [0.819, 0]]);
n.push(cylinder([178, 42, TOP], 8, 3));
n.push(disc(top(TOP + 3), [178, 42], 4).ink("mute"));
n.push(disc(top(TOP + 3), [178, 42], 4).accent().ink("none").alpha(flash));

// Paper slot.
n.push(plate(top(TOP), [120, 96, 184, 106], 5).fill("recess").ink("edge"));

// The hopper and the coins: each rests on the one below and bounces a little
// less; the floor swallows them when the month closes.
n.push(tube_back([HX, HY, TOP], 30, 22, 10));
for i in 0..4 {
    let start = starts[i]; let land = lands[i];
    let dz = keyed(t, [
        [start, 56.0 - 10.0 * i, "out"],
        [land, 0, "out"],
        [land + 0.0194, 2.5 * (1.0 - 0.25 * i), "in_out"],
        [land + 0.036, 0],
        [0.5417, 0, "in_out"],
        [0.6667, -30],
    ]);
    let a = keyed(t, [[start, 0, "out"], [start + 0.056, 1], [0.6722, 1], [0.6778, 0]]);
    let z = TOP + i * COIN_H + dz;
    n.push(cylinder([HX, HY, z], 16, COIN_H).accent().side_ink("accent_mute").alpha(a).layer(10 + i).clip_above(TOP));
    n.push(disc(top(z + COIN_H), [HX, HY], 11).ink("accent_mute").alpha(a).layer(10 + i).clip_above(TOP));
}
n.push(tube_front([HX, HY, TOP], 30, 22, 10));

// The invoices: last month's lifts away, this month's prints, its total
// marked once it is all out.
let sheets = [[keyed(t, [[0.55, 0, "out"], [0.65, 18]]), keyed(t, [[0.55, 1, "out"], [0.6333, 0]]), 1.0]];
if t > 0.6 {
    sheets.push([keyed(t, [[0.6722, -PAPER_H, "in_out"], [0.8472, 0]]), 1.0, keyed(t, [[0.84, 0, "out"], [0.9, 1]])]);
}
for s in sheets {
    let dz = s[0]; let a = s[1]; let total = s[2];
    if a <= 0.0 { continue; }
    let z0 = TOP + dz; let y = 101.2;
    n.push(slab([126, 100, z0], [52, 1.2, PAPER_H]).alpha(a).layer(1).clip_above(TOP));
    let lens = [30.0, 22.0, 26.0];
    for i in 0..3 {
        let z = z0 + PAPER_H - 12.0 - i * 7.0;
        n.push(line([[134, y, z], [134 + lens[i], y, z]]).alpha(a).layer(1).clip_above(TOP));
    }
    n.push(line([[134, y, z0 + 18], [150, y, z0 + 18]]).alpha(a).layer(1).clip_above(TOP));
    n.push(line([[134, y, z0 + 12], [170, y, z0 + 12]]).ink("accent").alpha(a * total).clip_above(TOP));
}

n
