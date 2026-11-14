// Arakun's raccoon peeks out of a dumpster in a back alley: it rises,
// grips the rim, turns its head one way and the other, blinks, flicks an
// ear and ducks back in. Its ringed tail sways and it breathes the whole
// time it is up.
//
// The raccoon is organic: ellipsoids merged into one body, with its mask,
// brows and eyes painted on as decals that turn with the head. The soft
// style lights it with gradients and grounds everything with soft shadows.
scene(#{
    name: "Raccoon",
    about: "Arakun's raccoon peeks out of its dumpster, looks around, blinks, and hides again.",
    seconds: 6.0,
    look: "paper",
    style: "soft",
});

let n = [];

// ---- palette ------------------------------------------------------------------------------
let FUR = "#8f8a84";      // warm gray coat
let FUR_DARK = "#3b3734"; // mask, tail rings, paws
let CREAM = "#f3eee6";    // cheeks, brows, muzzle
let EYE = "#f5a524";      // Arakun amber
let BIN = "#3f7d5a";      // dumpster green
let BIN_DARK = "#2e5c43";
let INSIDE = "#24382d";
let METAL = "#9aa0a6";
let CONCRETE = "#cfc9bf";
let TIRE = "#2b2b2e";

// ---- layout -------------------------------------------------------------------------------
let DW = 120.0; let DD = 84.0; let DH = 56.0;      // the dumpster
let RIM = 6.0; let TOP = DH + RIM;                 // its rim
let RX = 60.0; let RY = 60.0;                      // where the raccoon stands inside

// ---- the motion ------------------------------------------------------------------------
let up = keyed(t, [[0.03, 0, "out"], [0.17, 1], [0.8, 1, "in_out"], [0.93, 0]]);
let seen = clamp(up * 2.5, 0, 1);
// It faces three-quarters toward us and turns to look left, then right.
let look = -30.0 + keyed(t, [[0.22, 0, "in_out"], [0.3, 32], [0.44, 32, "in_out"], [0.54, -30], [0.66, -30, "in_out"], [0.74, 0]]);
let blink = keyed(t, [[0.36, 1, "in_out"], [0.375, 0.1], [0.39, 1]]) * keyed(t, [[0.7, 1, "in_out"], [0.715, 0.1], [0.73, 1]]);
let flick = keyed(t, [[0.58, 0, "out"], [0.6, 18], [0.64, 0]]);
let breath = 1.2 * sin(tau / 1.5 * 2.0 * PI());
let rise = DH - 34 + up * 38 + breath * up;        // height of the body's center

// ---- the alley floor, its shadows and the trash around ---------------------------------------
n.push(slab([-40, -40, -6], [DW + 80, DD + 80, 6], 14).color(CONCRETE));
for i in 1..4 {
    let x = -40.0 + i * (DW + 80) / 4.0;
    n.push(line([[x, -36, 0], [x, DD + 36, 0]]).ink("mute"));
}
n.push(shadow([DW / 2 + 6, DD / 2 + 6, 0], [DW * 0.7, DD * 0.74], 0.42));
// A pizza box on the ground at the back right.
n.push(shadow([DW + 22, 8, 0], [22, 20], 0.3));
n.push(slab([DW + 6, -10, 0], [30, 30, 4], 1.5).color("#d9b98a"));
n.push(plate(top(4), [DW + 10, -6, DW + 32, 16], 1).ink("mute"));

// ---- the dumpster's back half: lid, body, the dark inside, far rims -----------------------------
n.push(slab([2, -6, TOP], [DW - 4, 5, 46], 3).color(BIN_DARK));
for i in 0..4 {
    let x = centered(4, 10, 26, DW) + i * 26.0;
    n.push(plate(left(-1), [x, TOP + 8, x + 10, TOP + 38], 3).ink("mute"));
}
n.push(slab([0, 0, 0], [DW, DD, DH], 8).color(BIN));
n.push(plate(top(DH), [3, 3, DW - 3, DD - 3], 6).color(INSIDE).ink("mute"));
// Trash poking up inside: a can and a bag.
n.push(cylinder([22, 18, DH - 2], 5, 12).color("#c8423b"));
n.push(disc(top(DH + 10), [22, 18], 3.6).color(METAL).ink("mute"));
n.push(sphere([100, 20, DH + 4], 13).color("#26272b"));
n.push(slab([-3, -3, DH], [DW + 6, RIM, RIM], 3).color(METAL));          // back rim
n.push(slab([-3, -3, DH], [RIM, DD + 6, RIM], 3).color(METAL));          // left rim
n.push(shadow([RX + 6, RY - 4, DH], [30, 24], 0.35 * seen));

// ---- the raccoon ---------------------------------------------------------------------------------
let R = [];
// Tail: a chain of balls curling up behind it, ringed dark and gray; the
// tip sways more than the base.
for k in 0..9 {
    let u = k / 8.0;
    let sway = 7.0 * sin(tau / 1.5 * 2.0 * PI() - u * 2.2) * u;
    let p = [RX + 24 + u * 16 + sway, RY - 10 - u * 14, rise - 6 + u * 46];
    R.push(sphere(p, 9.5 - u * 3.0).color(if k % 2 == 1 || k == 8 { FUR_DARK } else { FUR }).merge(2));
}
// Body, ears, head, cheeks, tufts and snout: one merged body.
let body = ellipsoid([RX, RY, rise], [19, 16, 18]).color(FUR).merge(1);
let head = ellipsoid([RX + 2, RY + 6, rise + 26], [17, 14, 13]).yaw(look).color(FUR).merge(1);
R.push(body);
let ears = [];
for side in [-1.0, 1.0] {
    let a = if side < 0.0 { 0.0 } else { flick };
    let ear = ellipsoid(head.at([side * 0.62, -0.1, 0.92]), [6.5, 3, 9]).yaw(look + a).color(FUR);
    ears.push(ear);
    R.push(ear.merge(1));
}
R.push(head);
for side in [-1.0, 1.0] {
    R.push(sphere(head.at([side * 0.62, 0.48, -0.42]), 8).color(CREAM).merge(1));
    let tuft = head.at([side * 1.0, 0.3, -0.5]);
    let tip = head.at([side * 1.5, 0.3, -0.72]);
    let c = head.at([0, 0.3, -0.5]);
    R.push(ellipsoid(tuft, [2, 2, 4.5]).orient([tip[0] - c[0], tip[1] - c[1], tip[2] - c[2]]).color(CREAM).merge(1));
}
let snout = ellipsoid(head.at([0, 0.98, -0.32]), [5.5, 8, 4.8]).yaw(look).color(CREAM).merge(1);
R.push(snout);
// Painted on: inner ears, brows, the bandit mask, the nose stripe, the eyes.
for ear in ears {
    R.push(ear.decal([0, 1, 0], [-30, -38, 30, 34], 26).color(FUR_DARK).ink("none"));
}
for side in [-1.0, 1.0] {
    R.push(head.decal([0, 1, 0.3], [side * 26 - 18, 9, side * 26 + 18, 22], 13).color(CREAM).ink("none"));
}
for side in [-1.0, 1.0] {
    R.push(head.decal([0, 1, 0.3], [side * 27 - 22, -20, side * 27 + 22, 8], 14).color(FUR_DARK).ink("none"));
}
R.push(head.decal([0, 1, 0.3], [-6, -14, 6, 34], 5).color(FUR_DARK).ink("none"));
for side in [-1.0, 1.0] {
    let h = 8.0 * blink;
    R.push(head.decal([0, 1, 0.3], [side * 27 - 9, -6 - h, side * 27 + 9, -6 + h], 8).color(EYE).ink("none"));
    if blink > 0.6 {
        // Pupil and a catch light: the eyes look wet and alive.
        R.push(head.decal([0, 1, 0.3], [side * 27 - 3.5, -9, side * 27 + 3.5, -2], 3.5).color("#1d1b1a").ink("none"));
        R.push(head.decal([0, 1, 0.3], [side * 27 - 4, -3, side * 27 - 0.5, 1], 1.8).color("#ffffff").ink("none"));
    }
}
R.push(sphere(snout.at([0, 0.95, 0.45]), 2.3).color("#1d1b1a"));
for side in [-1.0, 1.0] {
    for w in 0..3 {
        let from = snout.at([side * 0.7, 0.55, -0.1 + w * 0.15]);
        let to = snout.at([side * 2.9, 0.75, -0.6 + w * 0.5]);
        R.push(line([from, to]).ink("mute"));
    }
}
for x in R { n.push(x.alpha(seen).layer(5)); }

// ---- the dumpster's near walls, over what hangs below its rim ------------------------------------
n.push(plate(left(DD), [0, 0, DW, DH], 8).color(BIN).ink("edge"));
n.push(plate(right(DW), [0, 0, DD, DH], 8).color(BIN_DARK).ink("edge"));
n.push(slab([-3, DD - 3, DH], [DW + 6, RIM, RIM], 3).color(METAL));      // front rim
n.push(slab([DW - 3, -3, DH], [RIM, DD + 6, RIM], 3).color(METAL));      // right rim
// Its arms reach over the rim and its dark paws hold on while it is up.
let paws = keyed(t, [[0.12, 0, "out"], [0.18, 1], [0.78, 1, "in_out"], [0.84, 0]]);
if paws > 0.0 {
    let arms = [];
    for side in [-1.0, 1.0] {
        let from = body.at([side * 0.55, 0.55, 0.15]);
        let to = [RX + side * 13, DD + 1, TOP + 2];
        let mid = [(from[0] + to[0]) / 2.0, (from[1] + to[1]) / 2.0, (from[2] + to[2]) / 2.0];
        let dir = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
        let len = sqrt(dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]);
        arms.push(ellipsoid(mid, [4.5, 4.5, len / 2.0 + 2]).orient(dir).color(FUR).merge(3));
        arms.push(ellipsoid(to, [5.5, 6, 3.4]).color(FUR_DARK).merge(3));
        for f in 0..3 {
            arms.push(sphere([to[0] - 3.5 + f * 3.5, to[1] + 4.5, to[2] - 1], 1.9).color(FUR_DARK).merge(3));
        }
    }
    for a in arms { n.push(a.alpha(paws).layer(6)); }
}
// Ribs pressed into the front, a label plate, the wheels.
for i in 0..5 {
    let x = centered(5, 6, 22, DW) + i * 22.0;
    n.push(plate(left(DD), [x, 8, x + 6, DH - 8], 2).color(BIN_DARK).ink("mute"));
}
n.push(plate(right(DW), [20, 18, DD - 20, 38], 4).color("#e8e2d6").ink("mute"));
n.push(text([DW, DD / 2, 28], "ARAKUN", 6, 0.5).ink("mute"));
for w in [[14, DD - 6], [DW - 14, DD - 6], [DW - 6, 14]] {
    n.push(shadow([w[0] + 2, w[1] + 2, -6], [9, 9], 0.3));
    n.push(cylinder([w[0], w[1], -6], 6, 6).color(TIRE));
}

n
