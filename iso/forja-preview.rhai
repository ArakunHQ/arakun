// Forja previews: each pull request rises out of its slot as its own live
// window, one after another, then they all sink back for the next round.
scene(#{
    name: "Forja · previews",
    about: "Every pull request gets its own live preview.",
    seconds: 5.4,
    look: "paper",
});

let Z0 = 8.0;
let W = 50.0; let H = 40.0; let D = 3.0;
let n = [];

n.push(slab([0, 0, 0], [220, 120, Z0], 14));
n.push(plate(top(Z0), [8, 8, 212, 112], 8).ink("mute").dashed());

// the branch: a trace that forks to each slot
let SY = 62.0;
n.push(line([[14, 100, Z0], [196, 100, Z0]]).ink("mute"));
for k in 0..3 {
    let x = 24.0 + k * 66.0 + W / 2.0;
    n.push(line([[x, 100, Z0], [x, SY + 8, Z0]]).ink("mute"));
    n.push(disc(top(Z0), [x, 100], 2.2).fill("ink").ink("none"));
}
n.push(line([[14, 100, Z0], [196, 100, Z0]]).ink("accent_mute").dashed().flow(tau / 1.35));

for k in 0..3 {
    let x0 = 24.0 + k * 66.0;
    let up = 0.08 + k * 0.12;
    let rise = keyed(t, [[up, -H, "out"], [up + 0.16, 0], [0.84 + k * 0.03, 0, "in_out"], [0.95 + k * 0.015, -H]]);
    let live = keyed(t, [[up + 0.14, 0, "out"], [up + 0.2, 1], [0.82, 1, "in_out"], [0.88, 0]]);
    // the slot
    n.push(plate(top(Z0), [x0 - 3, SY - 3, x0 + W + 3, SY + D + 3], 2.5).fill("recess").ink("edge"));
    // the window, rising out of it
    let z = Z0 + rise;
    let L = n.len();
    n.push(slab([x0, SY, z], [W, D, H], 2).clip_above(Z0));
    let f = left(SY + D);
    n.push(plate(f, [x0 + 3, z + H - 7, x0 + W - 3, z + H - 3], 1.5).fill("recess").ink("mute").clip_above(Z0));
    for j in 0..3 {
        n.push(disc(f, [x0 + 6 + j * 4.0, z + H - 5], 0.9).fill("mute").ink("none").clip_above(Z0));
    }
    n.push(plate(f, [x0 + 19, z + H - 6.4, x0 + W - 5, z + H - 3.6], 1.2).accent().ink("accent").alpha(live).layer(10 + k).clip_above(Z0));
    n.push(plate(f, [x0 + 5, z + 6, x0 + 22, z + H - 12], 2).ink("mute").clip_above(Z0));
    let lens = [20.0, 16.0, 22.0, 12.0];
    for j in 0..4 {
        let lz = z + H - 13.0 - j * 5.0;
        n.push(line([[x0 + 26, SY + D, lz], [x0 + 26 + lens[j], SY + D, lz]]).clip_above(Z0));
    }
}

n.push(label([24, 100, Z0], -34, "MAIN", 9));
n
