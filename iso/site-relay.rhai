// Relay: a sealed message leaves the phone, passes through the relay without
// being opened (the padlock never moves), and lands on the laptop running
// Arakun; the laptop answers with a sealed packet that rides back the same way.
scene(#{
    name: "Relay",
    about: "Phone and desktop talk through an end-to-end encrypted relay that cannot read what it carries.",
    seconds: 5.4,
    look: "arakun",
});

let Z0 = 8.0;                       // top of the base
let W = 500.0; let D = 176.0;       // base
let OUT_Y = 84.0; let BACK_Y = 106.0;   // the two lanes
// phone dock
let PX = 26.0; let PY = 66.0; let PW = 48.0; let PD = 58.0; let PH = 12.0;
// relay
let RX = 214.0; let RY = 50.0; let RW = 70.0; let RD = 90.0; let RH = 32.0;
let RT = Z0 + RH;
// laptop dock
let LX = 380.0; let LY = 44.0; let LW = 96.0; let LD = 102.0; let LH = 12.0;
let LT = Z0 + LH;
let TAU2 = 2.0 * PI();
let n = [];

fn packet(x, y, z, a, id) {
    // a sealed box: accent body, a seal band across the lid
    [slab([x - 6, y - 6, z], [12, 12, 9], 2).accent().alpha(a).layer(id),
     line([[x - 6, y, z + 9], [x + 6, y, z + 9]]).ink("accent_mute").alpha(a).layer(id),
     plate(top(z + 9), [x - 2.5, y - 2.5, x + 2.5, y + 2.5], 1.25).fill("accent_left").ink("accent_mute").alpha(a).layer(id)]
}

// ---- base -----------------------------------------------------------------------
n.push(slab([0, 0, 0], [W, D, Z0], 16));
n.push(plate(top(Z0), [10, 10, W - 10, D - 10], 10).ink("mute").dashed());
for c in [[22, 22], [W - 22, 22], [22, D - 22], [W - 22, D - 22]] {
    n.push(disc(top(Z0), c, 5).ink("edge"));
    n.push(disc(top(Z0), c, 2.4).fill("recess").ink("edge"));
}

// ---- lanes: out on one, back on the other -------------------------------------------
n.push(line([[PX + PW, OUT_Y, Z0], [LX, OUT_Y, Z0]]).ink("accent_mute").dashed().flow(t * 10.0));
n.push(line([[LX, BACK_Y, Z0], [PX + PW, BACK_Y, Z0]]).ink("mute").dashed().flow(t * 10.0));
// ties along the lanes
for i in 0..7 {
    let x = PX + PW + 14.0 + i * 18.0;
    n.push(line([[x, OUT_Y - 5, Z0], [x, OUT_Y + 5, Z0]]).ink("mute"));
    n.push(line([[x, BACK_Y - 5, Z0], [x, BACK_Y + 5, Z0]]).ink("mute"));
    let x2 = RX + RW + 12.0 + i * 12.5;
    n.push(line([[x2, OUT_Y - 5, Z0], [x2, OUT_Y + 5, Z0]]).ink("mute"));
    n.push(line([[x2, BACK_Y - 5, Z0], [x2, BACK_Y + 5, Z0]]).ink("mute"));
}

// ---- phone on its dock -----------------------------------------------------------
n.push(slab([PX, PY, Z0], [PW, PD, PH], 6));
let PZ = Z0 + PH;
n.push(plate(top(PZ), [PX + 5, PY + 5, PX + PW - 5, PY + PD - 5], 4));
// ports on the dock's right wall, one per lane
n.push(plate(right(PX + PW), [OUT_Y - 6, Z0 + 1.5, OUT_Y + 6, Z0 + 9.5], 2).fill("recess").ink("edge"));
n.push(plate(right(PX + PW), [BACK_Y - 6, Z0 + 1.5, BACK_Y + 6, Z0 + 9.5], 2).fill("recess").ink("edge"));
// the phone: an upright slab, screen facing down-left
let FX = PX + 10.0; let FY = PY + 26.0; let FW = 28.0; let FH = 54.0;
n.push(slab([FX, FY, PZ], [FW, 5, FH], 2));
let SY = FY + 5.0;
n.push(plate(left(SY), [FX + 2.5, PZ + 3, FX + FW - 2.5, PZ + FH - 3], 2.5).fill("recess").ink("edge"));
n.push(plate(left(SY), [FX + 10, PZ + FH - 6.5, FX + FW - 10, PZ + FH - 5], 0.75).fill("ink").ink("none"));
// chat bubbles on the screen
n.push(plate(left(SY), [FX + 5, PZ + 38, FX + 19, PZ + 43], 1.5).ink("mute"));
n.push(plate(left(SY), [FX + 9, PZ + 30, FX + FW - 5, PZ + 35], 1.5).ink("mute"));
// send button lights as the message leaves; answer card lights as it lands
let send = keyed(t, [[0.02, 0, "out"], [0.06, 1], [0.14, 1, "in_out"], [0.2, 0]]);
n.push(plate(left(SY), [FX + 5, PZ + 7, FX + FW - 5, PZ + 12], 2).ink("mute"));
if send > 0.0 {
    n.push(plate(left(SY), [FX + 5, PZ + 7, FX + FW - 5, PZ + 12], 2).accent().ink("accent").alpha(send).layer(2));
}
let got = keyed(t, [[0.87, 0, "out"], [0.9, 1], [0.96, 1, "in_out"], [1.0, 0]]);
if got > 0.0 {
    n.push(plate(left(SY), [FX + 5, PZ + 16, FX + FW - 5, PZ + 26], 2).ink("accent").alpha(got).layer(3));
    n.push(plate(left(SY), [FX + 8, PZ + 21, FX + 18, PZ + 23], 0.75).fill("accent").ink("none").alpha(got).layer(3));
}
// little status dots on the dock top
for i in 0..3 {
    n.push(disc(top(PZ), [PX + 12.0 + i * 6.0, PY + PD - 10], 1.3).fill("mute").ink("none"));
}

// ---- packets: where each one is on its lane over time --------------------------------
let RC = RX + RW / 2.0;
let pk = [];   // [x, y, alpha, id]
let DOOR_IN = RX - 26.0; let DOOR_OUT = RX + RW + 24.0;
for k in 0..2 {
    let s = 0.03 + k * 0.08;
    let x = keyed(t, [[s, PX + PW - 8, "out"], [s + 0.11, DOOR_IN, "in_out"], [s + 0.15, RC], [s + 0.2, RC, "in_out"], [s + 0.24, DOOR_OUT, "in_out"], [s + 0.34, LX + 12]]);
    let a = keyed(t, [[s, 0, "out"], [s + 0.03, 1], [s + 0.31, 1], [s + 0.34, 0]]);
    if a > 0.0 { pk.push([x, OUT_Y, a, 40 + k]); }
}
{
    let s = 0.55;
    let x = keyed(t, [[s, LX + 12, "out"], [s + 0.11, DOOR_OUT, "in_out"], [s + 0.15, RC], [s + 0.2, RC, "in_out"], [s + 0.24, DOOR_IN, "in_out"], [s + 0.34, PX + PW - 8]]);
    let a = keyed(t, [[s, 0, "out"], [s + 0.03, 1], [s + 0.31, 1], [s + 0.34, 0]]);
    if a > 0.0 { pk.push([x, BACK_Y, a, 45]); }
}
let BIG = 9999.0;
// behind the relay: between the phone dock and the relay's far wall
for p in pk {
    for s in packet(p[0], p[1], Z0, p[2], p[3]) {
        n.push(s.clip([PX + PW, -BIG, -BIG], [RX + 1, BIG, BIG]));
    }
}

// ---- the relay ---------------------------------------------------------------------
n.push(slab([RX, RY, Z0], [RW, RD, RH], 10));
n.push(plate(top(RT), [RX + 7, RY + 7, RX + RW - 7, RY + RD - 7], 6));
// vents down the left wall
for i in 0..7 {
    let x = RX + centered(7, 3, 7, RW) + i * 7.0;
    n.push(plate(left(RY + RD), [x, Z0 + 13, x + 3, Z0 + 25], 1.5).fill("recess").ink("mute"));
}
// ports on the right wall
n.push(plate(right(RX + RW), [OUT_Y - 7, Z0 + 1.5, OUT_Y + 7, Z0 + 10.5], 2).fill("recess").ink("edge"));
n.push(plate(right(RX + RW), [BACK_Y - 7, Z0 + 1.5, BACK_Y + 7, Z0 + 10.5], 2).fill("recess").ink("edge"));
// screws on the right wall
for y in [RY + 12, RY + RD - 12] {
    for z in [Z0 + 15, Z0 + RH - 6] {
        n.push(disc(right(RX + RW), [y, z], 1.6).ink("mute"));
    }
}
// the relay's pass-through light: on while a packet is inside
let lamp = keyed(t, [[0.15, 0, "out"], [0.18, 1], [0.31, 1, "in_out"], [0.35, 0], [0.67, 0, "out"], [0.7, 1], [0.75, 1, "in_out"], [0.79, 0]]);
for i in 0..4 {
    let y = RY + RD - 22.0 - i * 7.0;
    n.push(disc(top(RT), [RX + RW - 14, y], 1.8).fill("mute").ink("none"));
}
n.push(disc(top(RT), [RX + RW - 14, RY + RD - 22.0], 1.8).accent().ink("none").alpha(lamp).layer(30));

// the padlock on top: body, keyhole, and a shackle that never opens
let KX = RX + 16.0; let KY = RY + 30.0; let KW = 34.0; let KD = 16.0; let KH = 24.0;
let KZ = RT;
// shackle: a stadium ring standing in the body's middle plane, with a back
// copy for its thickness; the hole shows the relay's top behind it
let sc = KX + KW / 2.0;
let sy = KY + KD / 2.0;
let SB = KZ + KH - 8.0;
for yy in [sy - 2.5, sy + 2.0] {
    n.push(plate(left(yy), [sc - 12, SB, sc + 12, KZ + KH + 24], 12).fill("left").ink("edge"));
    n.push(plate(left(yy), [sc - 6.5, SB, sc + 6.5, KZ + KH + 18.5], 6.5).fill("top").ink("edge"));
}
n.push(slab([KX, KY, KZ], [KW, KD, KH], 4));
n.push(disc(left(KY + KD), [sc, KZ + 13], 3).fill("ink").ink("none"));
n.push(plate(left(KY + KD), [sc - 1, KZ + 5, sc + 1, KZ + 12], 0.5).fill("ink").ink("none"));

// ---- packets out of the relay toward the laptop ----------------------------------
for p in pk {
    for s in packet(p[0], p[1], Z0, p[2], p[3]) {
        n.push(s.clip([RX + RW, -BIG, -BIG], [LX + 1, BIG, BIG]));
    }
}

// ---- the laptop on its dock ----------------------------------------------------
n.push(slab([LX, LY, Z0], [LW, LD, LH], 8));
n.push(plate(right(LX + LW), [LY + 14, Z0 + 3, LY + LD - 14, Z0 + 8], 2).ink("mute"));
// laptop body
let AX = LX + 10.0; let AY = LY + 16.0; let AW = 76.0; let AD = 70.0;
n.push(slab([AX, AY, LT], [AW, AD, 4], 4));
let AT = LT + 4.0;
// keyboard: rows of keys, trackpad
for r in 0..4 {
    for c in 0..10 {
        let x = AX + centered(10, 5, 6.6, AW) + c * 6.6;
        let y = AY + 14.0 + r * 7.0;
        n.push(plate(top(AT), [x, y, x + 5, y + 5], 1).ink("mute"));
    }
}
n.push(plate(top(AT), [AX + 26, AY + 46, AX + AW - 26, AY + AD - 6], 2).ink("mute"));
// screen: an upright slab at the back of the body
let SZ = AT; let SH = 56.0;
n.push(slab([AX, AY, SZ], [AW, 4, SH], 3));
let SL = AY + 4.0;
n.push(plate(left(SL), [AX + 4, SZ + 4, AX + AW - 4, SZ + SH - 4], 3).fill("recess").ink("edge"));
// Arakun on the screen: a sidebar and lines that type as the message lands
n.push(plate(left(SL), [AX + 8, SZ + 8, AX + 20, SZ + SH - 8], 2).ink("mute"));
for i in 0..4 {
    n.push(plate(left(SL), [AX + 10.5, SZ + SH - 15.0 - i * 7.0, AX + 17.5, SZ + SH - 12.5 - i * 7.0], 1).fill("mute").ink("none"));
}
let lens = [34.0, 26.0, 40.0, 20.0];
for i in 0..4 {
    let on = 0.42 + i * 0.03;
    let w = lens[i] * keyed(t, [[on, 0, "out"], [on + 0.05, 1], [0.92, 1, "in_out"], [0.98, 0]]);
    let z = SZ + SH - 14.0 - i * 7.0;
    if w >= 3.0 {
        n.push(plate(left(SL), [AX + 25, z, AX + 25 + w, z + 2.5], 1).fill(if i == 3 { "accent" } else { "mute" }).ink("none"));
    }
}
n.push(disc(left(SL), [AX + AW / 2.0, SZ + SH - 2], 0.8).fill("ink").ink("none"));
// the laptop's power light breathes
let br = 0.5 - 0.5 * cos(t * 2.0 * TAU2);
n.push(disc(top(AT), [AX + AW - 6, AY + 8], 1.5).accent().ink("none").alpha(0.35 + 0.65 * br).layer(70));

// ---- callout -----------------------------------------------------------------------
n.push(label([sc + 12, sy + 2, KZ + KH + 10], 150, "THE RELAY CAN'T READ IT", 9));

n
