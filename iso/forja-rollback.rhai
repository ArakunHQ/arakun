// Forja rollback: four releases stand in the rack, the live one raised and
// lit. The rollback button flashes, the newest drops and the one before it
// rises into place; later the fix ships and the newest comes back up.
scene(#{
    name: "Forja · rollback",
    about: "Roll back to any release in one click.",
    seconds: 5.4,
    look: "paper",
});

let Z0 = 8.0;
let TT = 18.0;                     // top of the rack
let n = [];

n.push(slab([0, 0, 0], [200, 110, Z0], 12));
n.push(plate(top(Z0), [8, 8, 192, 102], 7).ink("mute").dashed());
n.push(slab([14, 30, Z0], [172, 40, TT - Z0], 6));

let a3 = keyed(t, [[0.3, 0, "in_out"], [0.44, 1], [0.76, 1, "in_out"], [0.9, 0]]);
let a4 = 1.0 - a3;
let live = [0.0, 0.0, a3, a4];

let start = 14.0 + centered(4, 34, 42, 172);
for k in 0..4 {
    let x = start + k * 42.0;
    n.push(plate(top(TT), [x - 2, 46, x + 36, 54], 2).fill("recess").ink("edge"));
}
for k in 0..4 {
    let x = start + k * 42.0;
    let z = TT - 30.0 + 14.0 * live[k];
    n.push(slab([x, 47, z], [34, 6, 46], 2).clip_above(TT));
    let f = left(53);
    n.push(plate(f, [x + 4, z + 36, x + 30, z + 42], 1.5).fill("recess").ink("mute").clip_above(TT));
    for j in 0..3 {
        let lz = z + 31.0 - j * 4.0;
        n.push(line([[x + 5, 53, lz], [x + 5 + [22.0, 14.0, 18.0][j], 53, lz]]).clip_above(TT));
    }
    if live[k] > 0.0 {
        n.push(plate(f, [x + 4, z + 36, x + 30, z + 42], 1.5).accent().ink("accent").alpha(live[k]).layer(10 + k).clip_above(TT));
        n.push(disc(top(z + 46), [x + 17, 50], 2).accent().ink("none").alpha(live[k]).layer(20 + k));
    }
}

// the rollback button on the front of the base
let flash = keyed(t, [[0.26, 0, "out"], [0.3, 1, "out"], [0.42, 0]]);
let press = keyed(t, [[0.26, 0, "out"], [0.29, -1.2, "in_out"], [0.34, 0]]);
n.push(cylinder([150, 90, Z0], 9, 3 + press));
n.push(disc(top(Z0 + 3 + press), [150, 90], 5).ink("mute"));
n.push(disc(top(Z0 + 3 + press), [150, 90], 5).accent().ink("none").alpha(flash).layer(40));
n.push(disc(top(Z0), [150, 90], 9 + 9.0 * flash).ink("accent_mute").alpha(flash).layer(41));
for i in 0..4 {
    n.push(disc(top(Z0), [40.0 + i * 8.0, 92], 1.3).fill("mute").ink("none"));
}

n.push(label([159, 90, Z0 + 3], 40, "ROLLBACK", 9));
n
