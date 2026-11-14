// EC2, explained in nine steps a five-year-old can follow. Each step has a
// caption (top left), names its parts with labels whose lines run from the
// words to the thing, and dims everything it is not about.
//
//   0 the zone · 1 the host · 2 the neighbours · 3 the image (AMI)
//   4 your instance is born · 5 its disk (EBS) · 6 visitors and the guard
//   7 scaling out · 8 turning it off
scene(#{
    name: "EC2 instances",
    about: "A guided tour of EC2: from a shared server and an image to a running instance, its disk, its guard, scaling and shutdown.",
    seconds: 27.0,
    look: "paper",
    // The tab's Guide mode stops 85% into each of the nine steps: every
    // step's story has played by then and its caption and labels are up.
    steps: 9,
    hold: 0.85,
});

let STEPS = 9;
let Z0 = 6.0;                 // top of the floor
let HT = 34.0;                // top of the host
let n = [];

// ---- layout --------------------------------------------------------------------------
let HX0 = 96.0; let HY0 = 56.0; let HX1 = 250.0; let HY1 = 170.0;   // the host
let SLOT_A = [HX0 + 10, HY0 + 10, HX0 + 80, HY0 + 62];     // your large instance
let SLOT_B = [HX0 + 90, HY0 + 10, HX1 - 10, HY0 + 52];     // the one that scales out
let SLOT_C = [HX0 + 10, HY0 + 72, HX0 + 80, HY1 - 10];     // another customer's
let SLOT_D = [HX0 + 90, HY0 + 62, HX1 - 10, HY1 - 10];     // free
let AMI = [40.0, 180.0];
let GW = [14.0, 70.0];
let EBS = [300.0, 150.0];
let A = [SLOT_A[0] + 3, SLOT_A[1] + 3]; let AW = SLOT_A[2] - SLOT_A[0] - 6; let AD = SLOT_A[3] - SLOT_A[1] - 6;
let B = [SLOT_B[0] + 3, SLOT_B[1] + 3]; let BW = SLOT_B[2] - SLOT_B[0] - 6; let BD = SLOT_B[3] - SLOT_B[1] - 6;
let IHA = 22.0; let IHB = 14.0;

let TO_A = [(SLOT_A[0] + SLOT_A[2]) / 2.0, (SLOT_A[1] + SLOT_A[3]) / 2.0];
let TO_B = [(SLOT_B[0] + SLOT_B[2]) / 2.0, (SLOT_B[1] + SLOT_B[3]) / 2.0];
let flight_a = [[AMI[0], AMI[1], Z0 + 14], [AMI[0], AMI[1], HT + 46], [TO_A[0], TO_A[1], HT + 46], [TO_A[0], TO_A[1], HT + 2]];
let flight_b = [[AMI[0], AMI[1], Z0 + 14], [AMI[0], AMI[1], HT + 58], [TO_B[0], TO_B[1], HT + 58], [TO_B[0], TO_B[1], HT + 2]];
let GATE = 68.0;              // x of the security group's gate on the way in
let net = [[GW[0] + 18, 98, Z0], [HX0, 98, Z0]];
let disk = [[EBS[0] - 18, EBS[1], Z0], [266, EBS[1], Z0], [258, 142, Z0], [HX1, 142, Z0]];

// ---- the steps ---------------------------------------------------------------------------
fn step_start(k, steps) { k * 1.0 / steps }
// How much step k is on screen: 1 inside it, crossfading at its edges.
fn weight(t, k, steps) {
    let s0 = k * 1.0 / steps; let s1 = (k + 1) * 1.0 / steps; let x = 0.008;
    // The last step hands over to the first as the loop closes.
    if k == 0 { return keyed(t, [[s1 - x, 1, "in_out"], [s1 + x, 0], [1.0 - 2.0 * x, 0, "in_out"], [1.0, 1]]); }
    if k == steps - 1 { return keyed(t, [[s0 - x, 0, "in_out"], [s0 + x, 1], [1.0 - 2.0 * x, 1, "in_out"], [1.0, 0]]); }
    keyed(t, [[s0 - x, 0, "in_out"], [s0 + x, 1], [s1 - x, 1, "in_out"], [s1 + x, 0]])
}
// How visible step k's caption is. Text never crossfades over text: the
// old caption is gone before the new one comes in.
fn caption_alpha(t, k, steps) {
    let s0 = k * 1.0 / steps; let s1 = (k + 1) * 1.0 / steps; let x = 0.007;
    keyed(t, [[s0, 0, "out"], [s0 + x, 1], [s1 - x, 1, "in_out"], [s1, 0]])
}
// Where in step k we are, 0..1.
fn local(t, k, steps) { progress(t, k * 1.0 / steps, (k + 1) * 1.0 / steps) }

// Which parts each step is about; the rest dims. "all" dims nothing.
let focus = [
    ["all"],
    ["host"],
    ["other"],
    ["ami", "slot"],
    ["mine"],
    ["mine", "ebs"],
    ["mine", "gw", "sg"],
    ["mine", "b"],
    ["all"],
];
let weights = [];
for k in 0..STEPS { weights.push(weight(t, k, STEPS)); }
// How bright a part is now: full when the step is about it, dim if not.
fn lit(part, focus, weights) {
    let a = 0.0;
    for k in 0..weights.len() {
        let f = focus[k];
        let on = f.contains("all") || f.contains(part);
        a += weights[k] * (if on { 1.0 } else { 0.28 });
    }
    a
}
// A group of shapes that fades as one picture.
fn group(list, a, l) {
    let out = [];
    for x in list { out.push(x.alpha(a).layer(l)); }
    out
}

// Beats of the story, on the loop's clock.
let s3 = step_start(3, STEPS); let s4 = step_start(4, STEPS); let s7 = step_start(7, STEPS); let s8 = step_start(8, STEPS);
let fly_a = progress(t, s3 + 0.03, s3 + 0.09);
let rise_a = keyed(t, [[s4, 0, "out"], [s4 + 0.03, 1], [s8 + 0.015, 1, "in_out"], [s8 + 0.05, 0]]);
let fly_b = progress(t, s7 + 0.008, s7 + 0.05);
let rise_b = keyed(t, [[s7 + 0.05, 0, "out"], [s7 + 0.075, 1], [s8 + 0.015, 1, "in_out"], [s8 + 0.05, 0]]);
let sg_on = keyed(t, [[step_start(6, STEPS), 0, "out"], [step_start(6, STEPS) + 0.02, 1], [s8 + 0.02, 1, "in_out"], [s8 + 0.05, 0]]);

// ---- the zone: floor and cables ----------------------------------------------------------
n.push(slab([0, 0, 0], [326, 214, Z0], 16));
// Idle motion, on the ambient clock so it runs at every stop of the guide.
// Each kind has its own period (all divide the 27 s loop) so they never
// beat together: dashes flow (2 per s), fans turn (1.5 s), things float
// (3 s / 4.5 s), the camera drifts (9 s / 13.5 s).
let FLOW = tau * 2.0;
// The zone's border drifts slowly around, like a fence line being walked.
n.push(plate(top(Z0), [10, 10, 316, 204], 10).ink("mute").flow(tau / 3.0));
// Cables carry their direction: dashes run from the internet, the image and
// the disk toward the server.
n.push(group([line(net).ink("mute").flow(FLOW), line([[GATE, 90, Z0], [GATE, 106, Z0]]).ink("mute")], lit("gw", focus, weights), 1));
n.push(group([line(disk).ink("mute").flow(FLOW)], lit("ebs", focus, weights), 2));
n.push(group([line([[AMI[0] + 22, AMI[1], Z0], [HX0, AMI[1] - 26, Z0]]).ink("mute").flow(FLOW)], lit("ami", focus, weights), 3));

// ---- the internet gateway ---------------------------------------------------------------------
let gw = [slab([GW[0], GW[1], Z0], [18, 56, 18], 5)];
for i in 0..5 {
    let y = GW[1] + 6.0 + i * 10.0;
    gw.push(plate(right(GW[0] + 18), [y, Z0 + 5, y + 5, Z0 + 13], 1.5).fill("recess").ink("mute"));
}
n.push(group(gw, lit("gw", focus, weights), 4));

// Visitors: packets from the internet, in a cycle on the ambient clock
// `tau` so they keep coming while the guide stops here. The guard (a door
// on the way in) sinks for the two the security group allows and stays shut
// for the third, which stops and gets a ×.
let s6 = step_start(6, STEPS);
let far = []; let near = [];
let lift = 0.0;
let cycle = (tau / 3.0) % 1.0;                 // 3 s: nine to a loop
let starts = [0.0, 0.2, 0.5];
for k in 0..3 {
    let u = progress(cycle, starts[k], starts[k] + 0.42);
    let blocked = k == 2;
    if !blocked {
        // Open while this one goes through, shut again after.
        let up = keyed(u, [[0.2, 0, "out"], [0.34, 1], [0.6, 1, "in_out"], [0.74, 0]]);
        if up > lift { lift = up; }
    }
    if sg_on > 0.0 && u > 0.0 && u < 1.0 {
        let path = if blocked { [net[0], [GATE - 24, 98, Z0]] } else { net };
        let p = along(path, ease("in_out", u));
        let f = sg_on * (if blocked { keyed(u, [[0, 0], [0.15, 1], [0.85, 1], [1, 0]]) } else { keyed(u, [[0, 0], [0.12, 1], [0.85, 1], [1, 0]]) });
        let pk = slab([p[0] - 4, p[1] - 4, Z0], [8, 8, 3], 2);
        let pk = if blocked { pk.alpha(f).layer(30 + k) } else { pk.accent().alpha(f).layer(30 + k) };
        if p[0] < GATE { far.push(pk); } else { near.push(pk); }
        if blocked && u > 0.55 {
            // The refusal, above the stopped packet, facing the viewer.
            let no = sg_on * keyed(u, [[0.55, 0, "out"], [0.65, 1], [0.85, 1], [1, 0]]);
            near.push(text([GATE - 24, 98, Z0 + 15], "×", 16, 0.5).alpha(no).layer(36));
        }
    }
}
n.push(far);
if sg_on > 0.0 {
    // Two pillars beside the lane and a door between them that sinks into
    // the floor to let a visitor through.
    let gate = [];
    gate.push(slab([GATE - 3, 80, Z0], [6, 5, 18], 1.5));
    gate.push(disc(left(85), [GATE, Z0 + 14], 1.2).accent().ink("none"));
    gate.push(slab([GATE - 1, 85, Z0 - lift * 15], [2, 26, 15], 0.8).accent().clip_above(Z0));
    gate.push(slab([GATE - 3, 111, Z0], [6, 5, 18], 1.5));
    n.push(group(gate, sg_on * lit("sg", focus, weights), 5));
}
n.push(near);

// ---- the host ------------------------------------------------------------------------------------
let host = [slab([HX0, HY0, Z0], [HX1 - HX0, HY1 - HY0, HT - Z0], 10)];
for i in 0..6 {
    let x = HX0 + 12.0 + i * 23.0;
    host.push(plate(left(HY1), [x, Z0 + 6, x + 16, Z0 + 20], 2).fill("recess").ink("mute"));
    // Drive lights flicker on their own rhythm: the server is always busy.
    let blink = (tau * 2.0 + i * 0.37) % 1.0;
    host.push(disc(left(HY1), [x + 13, Z0 + 23.5], 1.1).fill(if blink < 0.55 { "ink" } else { "mute" }).ink("none"));
}
// Two fans at the ends of the right wall, turning all the time, and vents
// between them.
let FAN_Z = (Z0 + HT) / 2.0;
for f in 0..2 {
    let cy = if f == 0 { HY0 + 18.0 } else { HY1 - 18.0 };
    host.push(disc(right(HX1), [cy, FAN_Z], 9).fill("recess").ink("mute"));
    let spin = tau / 1.5 * 2.0 * PI() * (if f == 0 { 1.0 } else { -1.0 });
    for b in 0..3 {
        let a = spin + b * 2.0 * PI() / 3.0;
        host.push(line([[HX1, cy, FAN_Z], [HX1, cy + 7.5 * cos(a), FAN_Z + 7.5 * sin(a)]]).ink("mute"));
    }
    host.push(disc(right(HX1), [cy, FAN_Z], 1.6).fill("mute").ink("none"));
}
for i in 0..5 {
    let y = HY0 + 36.0 + centered(5, 5, 9, HY1 - HY0 - 72.0) + i * 9.0;
    host.push(plate(right(HX1), [y, Z0 + 8, y + 5, HT - 8], 2).ink("mute"));
}
for s in [SLOT_A, SLOT_B, SLOT_C, SLOT_D] {
    host.push(plate(top(HT), s, 5).fill("recess").ink("mute"));
}
let host_lit = lit("host", focus, weights);
let slot_lit = lit("slot", focus, weights);
n.push(group(host, if slot_lit > host_lit { slot_lit } else { host_lit }, 6));
// The slots as moving outlines: all four while the server is explained
// (it is split into pieces), slot A as the image's destination and, at the
// end, as the place left free.
if weights[1] > 0.01 {
    for s in [SLOT_A, SLOT_B, SLOT_C, SLOT_D] {
        n.push(plate(top(HT + 0.05), s, 5).ink("accent").flow(FLOW).alpha(weights[1]).layer(14));
    }
}
let slot_a = weights[3] + weights[8];
if slot_a > 0.01 {
    n.push(plate(top(HT + 0.05), SLOT_A, 5).ink("accent").flow(FLOW).alpha(slot_a).layer(15));
}

// The other customer's instance: up the whole time, walled off from yours.
// While it is the subject it hovers a little off its slot, breathing.
let hover = weights[2] * 2.5 * (0.5 - 0.5 * cos(tau / 3.0 * 2.0 * PI()));
let other = [
    slab([SLOT_C[0] + 3, SLOT_C[1] + 3, HT + hover], [SLOT_C[2] - SLOT_C[0] - 6, SLOT_C[3] - SLOT_C[1] - 6, 16], 6),
    plate(top(HT + 16 + hover), [SLOT_C[0] + 12, SLOT_C[1] + 12, SLOT_C[2] - 12, SLOT_C[3] - 12], 3).ink("mute").flow(tau / 3.0),
];
// Its own lights: someone else's work, running all along.
for j in 0..3 {
    let on = 0.5 + 0.5 * sin((tau * 2.0 / 3.0 + j * 0.3) * 2.0 * PI());
    let mid = [(SLOT_C[0] + SLOT_C[2]) / 2.0, (SLOT_C[1] + SLOT_C[3]) / 2.0];
    other.push(disc(top(HT + 16 + hover), [mid[0] - 9 + j * 9.0, mid[1]], 1.6).fill("ink").ink("none").alpha(0.25 + 0.75 * on));
}
n.push(group(other, lit("other", focus, weights), 7));

// ---- your instances: they rise out of their slot and sink back into it -------------------------------
let mine = lit("mine", focus, weights);
// Your instance breathes while it is the subject of a step.
let bob_a = (weights[4] + weights[5] + weights[6]) * 1.5 * (0.5 - 0.5 * cos(tau / 4.5 * 2.0 * PI()));
let za = HT - IHA * (1.0 - rise_a) + bob_a;
if rise_a > 0.0 {
    let ia = [slab([A[0], A[1], za], [AW, AD, IHA], 6).accent().clip_above(HT)];
    // Two brains (vCPU) and four memory sticks (8 GB) on its top, the two
    // rows centered both ways.
    let top_a = za + IHA;
    let a_y = A[1] + (AD - (11 + 6 + 14)) / 2.0;
    for c in 0..2 {
        let on = s4 + 0.035 + c * 0.006;
        let l = keyed(t, [[on, 0, "out"], [on + 0.01, 1], [s8 + 0.003, 1], [s8 + 0.013, 0]]);
        let x0 = A[0] + centered(2, 11, 16, AW) + c * 16.0;
        ia.push(slab([x0, a_y, top_a], [11, 11, 1.6], 2).fill("top").ink("accent_mute").clip_above(HT + 0.1));
        // A heartbeat: a working brain never sits still.
        let beat = 0.7 + 0.3 * (0.5 + 0.5 * cos((tau + c * 0.5) * 2.0 * PI()));
        if l > 0.0 { ia.push(slab([x0, a_y, top_a], [11, 11, 1.6], 2).accent().alpha(l * beat).clip_above(HT + 0.1)); }
    }
    for m in 0..4 {
        let on = s4 + 0.05 + m * 0.005;
        let l = keyed(t, [[on, 0, "out"], [on + 0.01, 1], [s8 + 0.003, 1], [s8 + 0.013, 0]]);
        let x0 = A[0] + centered(4, 4, 8, AW) + m * 8.0;
        let r = [x0, a_y + 17, x0 + 4, a_y + 31];
        ia.push(plate(top(top_a), r, 1.2).fill("recess").ink("accent_mute").clip_above(HT + 0.1));
        // Memory in use: each stick fills and drains on its own rhythm.
        let periods = [3.0, 4.5, 2.7, 5.4];
        let used = 0.35 + 0.6 * (0.5 - 0.5 * cos((tau / periods[m] + m * 0.21) * 2.0 * PI()));
        let level = [r[0], r[3] - (r[3] - r[1]) * used, r[2], r[3]];
        if l > 0.0 { ia.push(plate(top(top_a), level, 1.2).accent().ink("accent").alpha(l).clip_above(HT + 0.1)); }
    }
    n.push(group(ia, mine, 8));
}
// The security group: a dashed fence around your instance.
if sg_on > 0.0 {
    n.push(plate(top(HT + 0.2), [SLOT_A[0] - 4, SLOT_A[1] - 4, SLOT_A[2] + 4, SLOT_A[3] + 4], 8).ink("accent").flow(-tau).alpha(sg_on * lit("sg", focus, weights)).layer(9));
}

let bob_b = weights[7] * 1.5 * (0.5 - 0.5 * cos((tau / 4.5 + 0.5) * 2.0 * PI()));
let zb = HT - IHB * (1.0 - rise_b) + bob_b;
if rise_b > 0.0 {
    let ib = [slab([B[0], B[1], zb], [BW, BD, IHB], 6).accent().clip_above(HT)];
    let top_b = zb + IHB;
    for c in 0..2 {
        let x0 = B[0] + centered(3, 11, 15, BW) + c * 15.0;
        let on = s7 + 0.075 + c * 0.006;
        let l = keyed(t, [[on, 0, "out"], [on + 0.01, 1], [s8 + 0.003, 1], [s8 + 0.013, 0]]);
        let y0 = B[1] + (BD - 11) / 2.0;
        ib.push(slab([x0, y0, top_b], [11, 11, 1.6], 2).fill("top").ink("accent_mute").clip_above(HT + 0.1));
        let beat = 0.7 + 0.3 * (0.5 + 0.5 * cos((tau + 0.25 + c * 0.5) * 2.0 * PI()));
        if l > 0.0 { ib.push(slab([x0, y0, top_b], [11, 11, 1.6], 2).accent().alpha(l * beat).clip_above(HT + 0.1)); }
    }
    // One memory stick (1 GB), clear of the rounded corner.
    // In the third place of the row, as tall as the top allows.
    let rx = B[0] + centered(3, 11, 15, BW) + 30 + 3.5;
    let ram = [rx, B[1] + 8, rx + 4, B[1] + BD - 8];
    ib.push(plate(top(top_b), ram, 1.2).fill("recess").ink("accent_mute").clip_above(HT + 0.1));
    let l = keyed(t, [[s7 + 0.078, 0, "out"], [s7 + 0.086, 1], [s8 + 0.003, 1], [s8 + 0.013, 0]]);
    if l > 0.0 { ib.push(plate(top(top_b), ram, 1.2).accent().ink("accent").alpha(l).clip_above(HT + 0.1)); }
    n.push(group(ib, lit("b", focus, weights), 10));
}

// ---- the EBS volume -------------------------------------------------------------------------------------
let ebs = [];
for k in 0..3 { ebs.push(cylinder([EBS[0], EBS[1], Z0 + k * 11.0], 18, 9.5)); }
ebs.push(disc(top(Z0 + 31.5), EBS, 8).ink("mute"));
// The top platter turns: a mark sweeps around it.
let sweep = tau / 1.5 * 2.0 * PI();
ebs.push(line([[EBS[0] + 9 * cos(sweep), EBS[1] + 9 * sin(sweep), Z0 + 31.5], [EBS[0] + 16 * cos(sweep), EBS[1] + 16 * sin(sweep), Z0 + 31.5]]).ink("edge"));
n.push(group(ebs, lit("ebs", focus, weights), 11));
// Data runs both ways between the volume and your instance from the EBS
// step until it shuts down, on the ambient clock so it flows at a stop too.
let s5 = step_start(5, STEPS);
let flowing = keyed(t, [[s5, 0, "out"], [s5 + 0.01, 1], [s8, 1, "in_out"], [s8 + 0.01, 0]]);
if flowing > 0.0 {
    for k in 0..3 {
        let u = (tau / 1.5 + k / 3.0) % 1.0;   // 1.5 s: eighteen to a loop
        let p = along(disk, ease("in_out", if k % 2 == 0 { u } else { 1.0 - u }));
        let f = flowing * keyed(u, [[0, 0], [0.15, 1], [0.85, 1], [1, 0]]);
        n.push(slab([p[0] - 4, p[1] - 4, Z0], [8, 8, 3], 2).accent().alpha(f).layer(40 + k));
    }
}

// ---- the AMI and the copies that fly to the host ------------------------------------------------------
let ami = [];
// While it is the subject its layers open apart and close again, an
// exploded view of what an image holds (system, programs, settings).
let spread = weights[3] * 5.0 * (0.5 - 0.5 * cos(tau / 3.0 * 2.0 * PI()));
for i in 0..3 { ami.push(slab([AMI[0] - 22, AMI[1] - 22, Z0 + i * (4.0 + spread)], [44, 44, 3], 8)); }
let ami_top = Z0 + 11 + 2.0 * spread;
ami.push(plate(top(ami_top), [AMI[0] - 12, AMI[1] - 12, AMI[0] + 12, AMI[1] + 12], 4).ink("mute"));
// The image lights up as each copy leaves it.
let stamp = keyed(fly_a, [[0, 0, "out"], [0.12, 1], [0.5, 0]]) + keyed(fly_b, [[0, 0, "out"], [0.12, 1], [0.5, 0]]);
if stamp > 0.0 {
    ami.push(plate(top(ami_top), [AMI[0] - 12, AMI[1] - 12, AMI[0] + 12, AMI[1] + 12], 4).accent().ink("accent").alpha(stamp));
}
n.push(group(ami, lit("ami", focus, weights), 12));
for f in [[fly_a, 70], [fly_b, 71]] {
    let u = f[0];
    if u > 0.0 && u < 1.0 {
        let p = along(if f[1] == 70 { flight_a } else { flight_b }, ease("in_out", u));
        let a = keyed(u, [[0, 0], [0.12, 1], [0.88, 1], [1, 0]]);
        n.push(slab([p[0] - 14, p[1] - 14, p[2]], [28, 28, 3], 5).accent().alpha(a).layer(f[1]));
    }
}

// ---- labels: for each step, what it names and from where ------------------------------------------------
// [step, anchor, length (negative: text on the left), text, delay in the step]
let labels = [
    [0, [326, 120, Z0], 40, "ZONA DE DISPONIBILIDAD"],
    [1, [HX1, HY0 + 18, HT], 120, "SERVIDOR FÍSICO"],
    [2, [SLOT_C[0] + 3, SLOT_C[3] - 3, HT + 16], -150, "OTRO CLIENTE"],
    [3, [AMI[0] - 22, AMI[1] + 22, Z0 + 11], -40, "AMI"],
    [4, [A[0] + AW, A[1], HT + IHA], 170, "M5.LARGE · 2 vCPU · 8 GB"],
    [5, [EBS[0] + 12.7, EBS[1] - 12.7, Z0 + 31.5], 95, "EBS · 100 GB"],
    [6, [GW[0], GW[1] + 56, Z0 + 18], -40, "INTERNET"],
    [6, [SLOT_A[2] + 4, SLOT_A[1] - 4, HT], 180, "GRUPO DE SEGURIDAD"],
    [7, [B[0] + BW, B[1], HT + IHB], 110, "T3.MICRO · 2 vCPU · 1 GB"],
    [8, [SLOT_A[2], SLOT_A[1], HT], 180, "ESPACIO LIBRE", 0.5],
];
for l in labels {
    // A label can wait for its moment: "free" only once the slot is empty.
    let d = if l.len() > 4 { l[4] } else { 0.0 };
    let u = progress(local(t, l[0], STEPS), d, 1.0);
    let show = keyed(u, [[0.04, 0, "out"], [0.14, 1], [0.9, 1], [0.97, 0]]);
    if show > 0.0 {
        let grow = keyed(u, [[0.1, 0, "out"], [0.32, 1], [0.84, 1, "in_out"], [0.94, 0]]);
        n.push(label(l[1], l[2], l[3], 8).reveal(grow).alpha(show).layer(90 + l[0]));
    }
}

// ---- captions: the tour, at the top left --------------------------------------------------------------
let captions = [
    ["UNA ZONA DE AWS", "Imagina un edificio gigante lleno de", "computadoras. Eso es una zona de AWS."],
    ["EL SERVIDOR", "Es una computadora enorme. Amazon la", "parte en pedacitos para prestarlos."],
    ["TUS VECINOS", "Este pedacito ya es de otra persona.", "Vive aquí, pero no puede ver lo tuyo."],
    ["LA RECETA: AMI", "Eliges una imagen: la receta de tu", "compu, con su sistema y sus programas."],
    ["¡NACE TU INSTANCIA!", "Tu propia compu dentro del servidor:", "2 cerebros (vCPU) y 8 GB de memoria."],
    ["TU DISCO: EBS", "Tus archivos viven en otro lado. Si tu", "compu se apaga, siguen a salvo."],
    ["VISITAS Y UN GUARDIA", "La gente llega por internet. El guardia", "(grupo de seguridad) decide quién pasa."],
    ["MÁS AYUDANTES", "¿Muchas visitas? Nace otra instancia", "para ayudar. A eso se le llama escalar."],
    ["APAGAR Y AHORRAR", "Si ya no la usas, la apagas y dejas", "de pagar. Tu disco EBS se queda."],
];
let cx = -200.0; let cy = -64.0;
// One card size for every step: as wide as the widest caption, with the
// same margin on both sides.
let card_w = 78.0 + STEPS * 7.0;
for c in captions {
    for w in [text_width(c[0], 11), text_width(c[1], 7.5), text_width(c[2], 7.5)] {
        if w > card_w { card_w = w; }
    }
}
card_w += 24.0;
for k in 0..STEPS {
    let w = caption_alpha(t, k, STEPS);
    if w > 0.0 {
        let c = captions[k];
        let lines = [
            // A card under the words, so the scene can pass behind them.
            card(at_screen(cx - 12, cy - 11), [card_w, 62], 8).ink("mute").fixed(),
            text(at_screen(cx, cy), `PASO ${k + 1} DE ${STEPS}`, 6.5).ink("mute").fixed(),
            text(at_screen(cx, cy + 13), c[0], 11).accent().fixed(),
            text(at_screen(cx, cy + 28), c[1], 7.5).fixed(),
            text(at_screen(cx, cy + 39), c[2], 7.5).fixed(),
        ];
        // Nine little marks: where in the tour you are.
        for j in 0..STEPS {
            let x = cx + 78 + j * 7.0;
            lines.push(text(at_screen(x, cy), if j == k { "●" } else { "○" }, 6.5).ink(if j == k { "edge" } else { "mute" }).fixed());
        }
        n.push(group(lines, w, 100 + k));
    }
}

// ---- the camera: closer to what each step is about ----------------------------------------------
// [focus, zoom] per step; the first and last show everything.
let shots = [
    [[163, 107, 20], 1.0],
    [[173, 113, 22], 1.12],
    [[141, 152, 40], 1.45],
    [[92, 136, 30], 1.18],
    [[141, 92, 52], 1.5],
    [[220, 121, 30], 1.22],
    [[104, 104, 24], 1.22],
    [[177, 90, 46], 1.38],
    [[163, 107, 20], 1.0],
];
// Moves glide from shot to shot, slower than the captions change.
fn glide(t, k, steps) {
    let s0 = k * 1.0 / steps; let s1 = (k + 1) * 1.0 / steps; let x = 0.022;
    if k == 0 { return keyed(t, [[s1 - x, 1, "in_out"], [s1 + x, 0], [1.0 - 2.0 * x, 0, "in_out"], [1.0, 1]]); }
    if k == steps - 1 { return keyed(t, [[s0 - x, 0, "in_out"], [s0 + x, 1], [1.0 - 2.0 * x, 1, "in_out"], [1.0, 0]]); }
    keyed(t, [[s0 - x, 0, "in_out"], [s0 + x, 1], [s1 - x, 1, "in_out"], [s1 + x, 0]])
}
let fx = 0.0; let fy = 0.0; let fz = 0.0; let zoom = 0.0; let total = 0.0;
for k in 0..STEPS {
    let w = glide(t, k, STEPS);
    let f = shots[k][0];
    fx += w * f[0]; fy += w * f[1]; fz += w * f[2]; zoom += w * shots[k][1]; total += w;
}
// A slow drift, so even a held stop is never a still photograph.
let drift = 1.0 + 0.015 * sin(tau / 9.0 * 2.0 * PI());
let sway = 2.0 * sin(tau / 13.5 * 2.0 * PI());
n.push(camera([fx / total + sway, fy / total - sway, fz / total], zoom / total * drift));

n
