import SwiftUI

// Arakun's raccoon, alive: the desktop's mascot (core/src/mascot.rs) cut down
// for the phone. The same drawing (assets/brand/mascot.json) on springs, with
// eyes that blink and look around, and one pose and one symbol per mood, so
// each state reads at a glance. With Reduce Motion it holds still.

/// What the agent behind the raccoon is doing.
enum Mood: Equatable {
    case idle, thinking, working, waiting, done, error, sleeping
}

/// The drawing's parts, read once from the bundle.
private struct MascotArt {
    let head: Path
    let ink: Path
    /// The head with the ink parts as holes, for an even-odd fill.
    let body: Path
    let eyes: [CGRect]

    static let full = MascotArt(small: false)
    static let small = MascotArt(small: true)

    /// The nose, which mascot.json leaves out: the brand SVG's, moved by the
    /// same (10.73, 10.68) as the rest.
    static let nose: Path = {
        var p = Path()
        p.move(to: CGPoint(x: 99.89, y: 96.42))
        p.addCurve(to: CGPoint(x: 89.27, y: 105.06), control1: CGPoint(x: 99.89, y: 99.69), control2: CGPoint(x: 95.14, y: 105.06))
        p.addCurve(to: CGPoint(x: 78.65, y: 96.42), control1: CGPoint(x: 83.40, y: 105.06), control2: CGPoint(x: 78.65, y: 99.70))
        p.addCurve(to: CGPoint(x: 89.27, y: 90.49), control1: CGPoint(x: 78.65, y: 93.14), control2: CGPoint(x: 83.40, y: 90.49))
        p.addCurve(to: CGPoint(x: 99.89, y: 96.42), control1: CGPoint(x: 95.14, y: 90.49), control2: CGPoint(x: 99.89, y: 93.14))
        p.closeSubpath()
        return p.applying(CGAffineTransform(translationX: 10.73, y: 10.68))
    }()

    init(small: Bool) {
        let url = Bundle.main.url(forResource: "mascot", withExtension: "json")
        let json = url.flatMap { try? Data(contentsOf: $0) }.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] } ?? [:]
        func paths(_ key: String) -> [String] {
            if let list = json[key] as? [[String: Any]] { return list.compactMap { $0["d"] as? String } }
            if let one = json[key] as? [String: Any], let d = one["d"] as? String { return [d] }
            return []
        }
        let suffix = small ? "_small" : ""
        let headKey = small ? "head_small" : "head"
        var head = Path()
        for d in paths(headKey) { head.addPath(Path(svg: d)) }
        // The dark of the mask, the ear insides and the forehead stripe.
        var ink = Path()
        for key in ["band", "ear_in", "stripe"] {
            for d in paths(key + suffix).isEmpty ? paths(key) : paths(key + suffix) { ink.addPath(Path(svg: d)) }
        }
        ink.addPath(Self.nose)
        self.head = head
        self.ink = ink
        var body = head
        body.addPath(ink)
        self.body = body
        eyes = (json["eyes"] as? [[String: Double]] ?? []).map {
            CGRect(x: ($0["cx"] ?? 0) - ($0["w"] ?? 0) / 2, y: ($0["cy"] ?? 0) - ($0["h"] ?? 0) / 2, width: $0["w"] ?? 0, height: $0["h"] ?? 0)
        }
    }
}

extension Path {
    /// An SVG path of M, L, C and Z, the commands the mascot's drawing uses.
    init(svg d: String) {
        self.init()
        var nums: [CGFloat] = []
        var cmd: Character = "M"
        var path = Path()
        func flush() {
            switch cmd {
            case "M":
                var i = 0
                while i + 1 < nums.count {
                    let p = CGPoint(x: nums[i], y: nums[i + 1])
                    if i == 0 { path.move(to: p) } else { path.addLine(to: p) }
                    i += 2
                }
            case "L":
                var i = 0
                while i + 1 < nums.count { path.addLine(to: CGPoint(x: nums[i], y: nums[i + 1])); i += 2 }
            case "C":
                var i = 0
                while i + 5 < nums.count {
                    path.addCurve(to: CGPoint(x: nums[i + 4], y: nums[i + 5]),
                                  control1: CGPoint(x: nums[i], y: nums[i + 1]),
                                  control2: CGPoint(x: nums[i + 2], y: nums[i + 3]))
                    i += 6
                }
            case "Z": path.closeSubpath()
            default: break
            }
            nums = []
        }
        var token = ""
        func endNumber() {
            if let v = Double(token) { nums.append(CGFloat(v)) }
            token = ""
        }
        for ch in d {
            if "MLCZ".contains(ch) {
                endNumber(); flush(); cmd = ch
            } else if ch == "-" {
                if !token.isEmpty, token.last != "e" { endNumber() }
                token.append(ch)
            } else if ch == "." && token.contains(".") {
                endNumber(); token = "."
            } else if ch == " " || ch == "," {
                endNumber()
            } else {
                token.append(ch)
            }
        }
        endNumber(); flush()
        self = path
    }
}

/// A value that follows its target like a spring.
private struct Spring {
    var x: CGFloat
    var v: CGFloat = 0

    init(_ x: CGFloat) { self.x = x }

    mutating func step(to target: CGFloat, dt: CGFloat, stiffness: CGFloat = 170, damping: CGFloat = 16) {
        v += (stiffness * (target - x) - damping * v) * dt
        x += v * dt
    }
}

/// One frame's intent, as on the desktop.
private struct Pose {
    /// x, y, rotation in degrees, scale x, scale y.
    var head: [CGFloat] = [0, 0, 0, 1, 1]
    var look: [CGFloat] = [0, 0]
    /// Eye width and height factors, and tilt in degrees (mirrored per eye).
    var eye: [CGFloat] = [1, 1, 0]
    var eyes: Eyes = .open
    var blink = true
    var prop: Prop?

    enum Eyes { case open, happy, cross }
    enum Prop { case dots, ask, bang, check, zzz }
}

/// The springs and clocks behind one raccoon, kept across frames.
@MainActor
private final class MascotState {
    var head = [Spring(0), Spring(0), Spring(0), Spring(1), Spring(1)]
    var look = [Spring(0), Spring(0)]
    var eye = [Spring(1), Spring(1), Spring(0)]
    var propAlpha: CGFloat = 0
    var prop: Pose.Prop?
    var mood: Mood = .idle
    var moodStart: TimeInterval = 0
    var last: TimeInterval?
    var nextBlink: TimeInterval = 1.5
    var blinkStart: TimeInterval?
    var nextGlance: TimeInterval = 2
    var glance: [CGFloat] = [0, 0]
    var hopStart: TimeInterval?
    /// A mood that was busy and turns idle plays `done` first.
    func enter(_ m: Mood, at now: TimeInterval) {
        guard m != mood else { return }
        let busy = mood == .working || mood == .thinking || mood == .waiting
        mood = (m == .idle && busy) ? .done : m
        moodStart = now
    }
}

/// The raccoon, `width` points wide, in a mood. A tap makes it hop.
struct Mascot: View {
    var mood: Mood = .idle
    var width: CGFloat = 96
    /// Room above the head for the mood's symbol.
    var props = true
    @State private var state = MascotState()
    @Environment(\.accessibilityReduceMotion) private var still
    @Environment(\.colorScheme) private var scheme

    /// The box in mascot.json's units, which put the brand SVG's head (178.54
    /// wide) at (10.73, 10.68): room for hops, and props float up to about 50
    /// units above it.
    private static let box = CGRect(x: 5.73, y: 2.68, width: 188.54, height: 152)
    private static let propRoom: CGFloat = 52
    /// The head's centre line and chin; it squashes and turns about the chin.
    private static let cx: CGFloat = 100
    private static let floor: CGFloat = 147.84
    /// Idle this long and it falls asleep, until a tap or a new mood.
    private static let dozeAfter: TimeInterval = 120

    private var small: Bool { width < 48 }
    private var top: CGFloat { props && !small ? Self.propRoom : 0 }
    private var height: CGFloat { width * (Self.box.height + top) / Self.box.width }

    var body: some View {
        TimelineView(.animation(minimumInterval: 1 / 60, paused: still)) { ctx in
            Canvas { g, size in
                draw(&g, size: size, now: ctx.date.timeIntervalSinceReferenceDate)
            }
        }
        .frame(width: width, height: height)
        .contentShape(Rectangle())
        .onTapGesture {
            let now = Date().timeIntervalSinceReferenceDate
            state.hopStart = now
            if state.mood == .idle { state.moodStart = now }
        }
        .accessibilityElement()
        .accessibilityLabel("Arakun")
        .accessibilityValue(moodWord)
    }

    private var moodWord: String {
        switch mood {
        case .idle: "Idle"
        case .thinking: "Thinking"
        case .working: "Working"
        case .waiting: "Waiting for you"
        case .done: "Done"
        case .error: "Error"
        case .sleeping: "Asleep"
        }
    }

    // ---- motion ----------------------------------------------------------

    private func pose(_ s: MascotState, now: TimeInterval) -> Pose {
        var p = Pose()
        let t = now - s.moodStart
        switch s.mood {
        case .idle where t > Self.dozeAfter && s.hopStart == nil:
            // Nothing has happened for a while: it dozes off.
            let b = sin(t * 1.3)
            p.eye = [1.2, 0.13, 0]
            p.head[1] = 7 + 1.5 * b
            p.head[2] = -3
            p.look = [0, 0.5]
            p.blink = false
            p.prop = .zzz
        case .idle:
            if now > s.nextGlance {
                s.glance = Bool.random() ? [CGFloat.random(in: -0.8...0.8), CGFloat.random(in: -0.5...0.4)] : [0, 0]
                s.nextGlance = now + .random(in: 1.6...4.2)
            }
            p.look = s.glance
            p.head[1] = 1.2 * sin(now * 1.6)
        case .thinking:
            let side: CGFloat = Int(t / 3.2) % 2 == 1 ? -1 : 1
            p.head[2] = 8 * side
            p.head[1] = -1
            p.eye[1] = 0.92
            p.look = [0.75 * side + 0.1 * sin(now * 1.3), -0.95]
            p.prop = .dots
        case .working:
            // Head down, narrowed eyes reading line after line.
            let line = t.truncatingRemainder(dividingBy: 1.6) / 1.6
            let lx = -0.75 + 1.5 * min(1, line / 0.85)
            if Int(t / 1.6) % 5 == 4 && line > 0.2 {
                p.look = [0, -0.1]
            } else {
                p.look = [lx, 0.85]
                p.eye = [1, 0.55, -9]
                p.head[1] = 4
                p.head[2] = lx * 1.5
            }
        case .waiting:
            // Big eyes on you and a hop that keeps coming back.
            let c = t.truncatingRemainder(dividingBy: 2)
            p.head[2] = Int(t / 2) % 2 == 1 ? -5 : 5
            if c < 0.12 { p.head[1] = 2; p.head[3] = 1.05; p.head[4] = 0.92 }
            else if c < 0.45 { p.head[1] = -12; p.head[3] = 0.97; p.head[4] = 1.05 }
            else if c < 0.58 { p.head[1] = 1; p.head[3] = 1.04; p.head[4] = 0.94 }
            p.eye = [1.12, 1.15, 0]
            p.prop = .ask
        case .done:
            if t > 2.8 { s.mood = .idle; s.moodStart = now; return pose(s, now: now) }
            p.prop = .check
            if t < 0.18 { p.head[1] = 3; p.head[3] = 1.07; p.head[4] = 0.88; p.eye = [1.15, 1.2, 0] }
            else if t < 0.55 { p.head[1] = -16; p.head[3] = 0.96; p.head[4] = 1.06; p.eyes = .happy }
            else if t < 0.7 { p.head[1] = 1; p.head[3] = 1.05; p.head[4] = 0.93; p.eyes = .happy }
            else if t < 2.5 { p.eyes = .happy; p.head[2] = 5 * sin((t - 0.7) * 6) }
        case .error:
            // A shake with crossed eyes, then a sad look down.
            p.prop = .bang
            if t < 0.8 {
                p.head[0] = 8 * sin(t * 42) * exp(-3.5 * t)
                p.eyes = .cross
            } else {
                p.eye = [1, 0.72, 17]
                p.head[1] = 6
                p.look = [0, 0.75]
            }
        case .sleeping:
            let b = sin(t * 1.3)
            p.eye = [1.2, 0.13, 0]
            p.head[1] = 7 + 1.5 * b
            p.head[2] = -3
            p.head[4] = 1 + 0.03 * b
            p.look = [0, 0.5]
            p.blink = false
            p.prop = .zzz
        }
        // A tap: one hop with happy eyes.
        if let h = s.hopStart {
            let u = now - h
            if u > 0.75 { s.hopStart = nil }
            else if u < 0.12 { p.head[1] = 3; p.head[3] = 1.06; p.head[4] = 0.9 }
            else if u < 0.45 { p.head[1] = -14; p.head[3] = 0.97; p.head[4] = 1.05; p.eyes = .happy }
            else { p.eyes = .happy }
        }
        return p
    }

    private func step(_ s: MascotState, now: TimeInterval) -> (Pose, blink: CGFloat) {
        if s.last == nil { s.moodStart = now; s.mood = mood }
        s.enter(mood, at: now)
        let dt = CGFloat(min(0.05, now - (s.last ?? now - 1 / 60)))
        s.last = now
        let p = still ? Pose() : pose(s, now: now)
        for i in 0..<5 { s.head[i].step(to: p.head[i], dt: dt, stiffness: i == 0 ? 400 : 190, damping: i == 0 ? 14 : 15) }
        for i in 0..<2 { s.look[i].step(to: p.look[i], dt: dt, stiffness: 260, damping: 24) }
        for i in 0..<3 { s.eye[i].step(to: p.eye[i], dt: dt, stiffness: 300, damping: 24) }
        if s.prop != p.prop {
            s.propAlpha = max(0, s.propAlpha - dt * 6)
            if s.propAlpha == 0 { s.prop = p.prop }
        } else if p.prop != nil {
            s.propAlpha = min(1, s.propAlpha + dt * 4)
        }
        var blink: CGFloat = 1
        if p.blink && !still {
            if now > s.nextBlink, s.blinkStart == nil {
                s.blinkStart = now
                s.nextBlink = now + .random(in: 2.2...5.5)
            }
            if let b = s.blinkStart {
                let u = (now - b) / 0.2
                if u >= 1 { s.blinkStart = nil } else { blink = max(0.08, abs(1 - 2 * u)) }
            }
        }
        return (p, blink)
    }

    // ---- paint -----------------------------------------------------------

    /// Paper fur and ink holes on every ground, as the brand's marks and the
    /// desktop's mascot draw it.
    private let fur = Color(UIColor(hex: 0xF1EFE8))
    private let inkColor = Color(UIColor(hex: 0x151515))
    /// The symbols over the head: in the text color, so they read on white too.
    private var propColor: Color { scheme == .dark ? fur : Color(UIColor(hex: 0x151515)) }

    private func draw(_ g: inout GraphicsContext, size: CGSize, now: TimeInterval) {
        let s = state
        let (p, blink) = step(s, now: now)
        let art = small ? MascotArt.small : MascotArt.full
        let k = size.width / Self.box.width
        g.translateBy(x: -Self.box.minX * k, y: (top - Self.box.minY) * k)
        g.scaleBy(x: k, y: k)

        var head = g
        // Squash and turn about the chin.
        head.translateBy(x: Self.cx + s.head[0].x, y: Self.floor + s.head[1].x)
        head.rotate(by: .degrees(Double(s.head[2].x)))
        head.scaleBy(x: s.head[3].x, y: s.head[4].x)
        head.translateBy(x: -Self.cx, y: -Self.floor)
        // The ink parts alone, then the head with them as holes, as the
        // desktop paints it: nothing shows behind the head on any ground.
        head.fill(art.ink, with: .color(inkColor))
        head.fill(art.body, with: .color(fur), style: FillStyle(eoFill: true))

        let amber = Color(UIColor(hex: 0xFFB547))
        let bigger: CGFloat = small ? 1.3 : 1
        for (i, r) in art.eyes.enumerated() {
            let mirror: CGFloat = i == 0 ? 1 : -1
            let w = r.width * s.eye[0].x * bigger
            let h = r.height * max(0.06, s.eye[1].x) * blink * bigger
            let c = CGPoint(x: r.midX + s.look[0].x * 4.5, y: r.midY + s.look[1].x * 4)
            var e = head
            e.translateBy(x: c.x, y: c.y)
            e.rotate(by: .degrees(Double(s.eye[2].x * mirror)))
            switch p.eyes {
            case .open:
                e.fill(Path(roundedRect: CGRect(x: -w / 2, y: -h / 2, width: w, height: h), cornerRadius: min(w, h) / 2), with: .color(amber))
            case .happy:
                var arc = Path()
                arc.move(to: CGPoint(x: -w * 0.75, y: 4))
                arc.addQuadCurve(to: CGPoint(x: w * 0.75, y: 4), control: CGPoint(x: 0, y: -10))
                e.stroke(arc, with: .color(amber), style: StrokeStyle(lineWidth: 5.5, lineCap: .round))
            case .cross:
                var x = Path()
                x.move(to: CGPoint(x: -7, y: -7)); x.addLine(to: CGPoint(x: 7, y: 7))
                x.move(to: CGPoint(x: 7, y: -7)); x.addLine(to: CGPoint(x: -7, y: 7))
                e.stroke(x, with: .color(amber), style: StrokeStyle(lineWidth: 5.5, lineCap: .round))
            }
        }

        if top > 0, let prop = s.prop, s.propAlpha > 0.02 {
            var m = g
            m.opacity = Double(s.propAlpha)
            let anchor = CGPoint(x: Self.cx + s.head[0].x * 0.5, y: -19.3 + s.head[1].x * 0.6)
            m.translateBy(x: anchor.x, y: anchor.y)
            m.scaleBy(x: 0.6 + 0.4 * s.propAlpha, y: 0.6 + 0.4 * s.propAlpha)
            drawProp(prop, in: &m, now: now - s.moodStart, color: prop == .ask ? Color.amberInk : propColor)
        }
    }

    private func drawProp(_ prop: Pose.Prop, in g: inout GraphicsContext, now t: TimeInterval, color: Color) {
        let line = StrokeStyle(lineWidth: 7, lineCap: .round, lineJoin: .round)
        switch prop {
        case .dots:
            for i in 0..<3 {
                let u = max(0, sin(t * 5.2 - Double(i) * 0.95))
                var d = g
                d.opacity *= 0.4 + 0.6 * u
                d.fill(Path(ellipseIn: CGRect(x: -21 + 21 * CGFloat(i) - 7, y: -9 * u - 7, width: 14, height: 14)), with: .color(color))
            }
        case .ask:
            var a = g
            a.translateBy(x: 0, y: -7 * abs(sin(t * .pi)))
            a.rotate(by: .degrees(6 * sin(t * 3.1)))
            var hook = Path()
            for k in 0...14 {
                let u = -Double.pi + Double(k) / 14 * 1.3 * .pi
                let p = CGPoint(x: 9 * cos(u), y: -9 + 9 * sin(u))
                if k == 0 { hook.move(to: p) } else { hook.addLine(to: p) }
            }
            hook.addLine(to: CGPoint(x: 0, y: 3))
            a.stroke(hook, with: .color(color), style: line)
            a.fill(Path(ellipseIn: CGRect(x: -4.5, y: 8.5, width: 9, height: 9)), with: .color(color))
        case .bang:
            var b = g
            if t < 0.8 { b.translateBy(x: 5 * sin(t * 42) * exp(-3 * t), y: 0) }
            var bar = Path()
            bar.move(to: CGPoint(x: 0, y: -18)); bar.addLine(to: CGPoint(x: 0, y: 1))
            b.stroke(bar, with: .color(color), style: StrokeStyle(lineWidth: 8, lineCap: .round))
            b.fill(Path(ellipseIn: CGRect(x: -4.5, y: 7.5, width: 9, height: 9)), with: .color(color))
        case .check:
            let pts = [CGPoint(x: -15, y: 0), CGPoint(x: -5, y: 11), CGPoint(x: 16, y: -12)]
            var tick = Path()
            tick.addLines(pts)
            g.stroke(tick.trimmedPath(from: 0, to: min(1, t / 0.3)), with: .color(color), style: StrokeStyle(lineWidth: 9, lineCap: .round, lineJoin: .round))
        case .zzz:
            // Three z's drifting up and fading, one after the other.
            for i in 0..<3 {
                let u = (t * 0.45 + Double(i) / 3).truncatingRemainder(dividingBy: 1)
                let size = 8 + 8 * u
                var z = g
                z.opacity *= sin(u * .pi)
                z.translateBy(x: 4 + 48 * u, y: 12 - 40 * u)
                var p = Path()
                p.move(to: CGPoint(x: -size / 2, y: -size / 2))
                p.addLine(to: CGPoint(x: size / 2, y: -size / 2))
                p.addLine(to: CGPoint(x: -size / 2, y: size / 2))
                p.addLine(to: CGPoint(x: size / 2, y: size / 2))
                z.stroke(p, with: .color(color), style: StrokeStyle(lineWidth: 4, lineCap: .round, lineJoin: .round))
            }
        }
    }
}

#if DEBUG
#Preview {
    VStack(spacing: 24) {
        HStack { Mascot(mood: .idle, width: 120); Mascot(mood: .working, width: 120) }
        HStack { Mascot(mood: .waiting, width: 120); Mascot(mood: .sleeping, width: 120) }
    }
    .padding()
    .background(Color.ground)
}
#endif

#if DEBUG
/// Every mood side by side, for design work: launch with `-demo -demoMoods YES`.
struct MoodGallery: View {
    private let moods: [(Mood, String)] = [(.idle, "Idle"), (.thinking, "Thinking"), (.working, "Working"),
                                           (.waiting, "Waiting"), (.done, "Done"), (.error, "Error"), (.sleeping, "Sleeping")]

    var body: some View {
        ScrollView {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 150))], spacing: 28) {
                ForEach(moods, id: \.1) { mood, name in
                    VStack(spacing: 8) {
                        Mascot(mood: mood, width: 120)
                        Eyebrow(name)
                    }
                }
                VStack(spacing: 8) {
                    Mascot(mood: .waiting, width: 40)
                    Eyebrow("40 pt")
                }
            }
            .padding(20)
        }
        .background(Color.ground.ignoresSafeArea())
    }
}
#endif
