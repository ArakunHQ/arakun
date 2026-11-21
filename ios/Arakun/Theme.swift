import SwiftUI
import UIKit

// The brand book's tokens (.claude/skills/arakun-brand/reference.md): black
// and white grounds with no tint, one amber accent, Geist and Geist Mono.
// Every view takes its colors, type and corners from here.

extension Color {
    /// A color with a dark-book and a light-book value.
    private static func pair(_ dark: UInt32, _ light: UInt32) -> Color {
        Color(UIColor { $0.userInterfaceStyle == .dark ? UIColor(hex: dark) : UIColor(hex: light) })
    }

    /// The page background.
    static let ground = pair(0x0B0B0B, 0xFFFFFF)
    /// Cards, sheets and menus.
    static let raised = pair(0x161616, 0xF7F7F7)
    /// Wells and code.
    static let deep = pair(0x050505, 0xF2F2F2)
    static let line = pair(0x1F1F1F, 0xEBEBEB)
    static let lineStrong = pair(0x2B2B2B, 0xDCDCDC)
    static let text = pair(0xF2F2F2, 0x0B0B0B)
    static let textMuted = pair(0xA3A3A3, 0x595959)
    static let textFaint = pair(0x737373, 0x757575)
    /// The one accent, as a fill or a mark.
    static let amber = Color(UIColor(hex: 0xFFB547))
    /// Amber as text or a thin line: darker on white so it stays readable.
    static let amberInk = pair(0xFFB547, 0x995C00)
    /// Amber behind something, faintly.
    static let amberSoft = pair(0x2A1F0D, 0xFFF1DB)
    /// Text on an amber fill.
    static let onAmber = Color(UIColor(hex: 0x151515))

    // Status: state only, always with a word beside it.
    static let success = pair(0x3FB950, 0x1A7F37)
    static let failure = pair(0xF85149, 0xCF222E)
    static let attention = pair(0xD29922, 0x9A6700)
    static let merged = Color(UIColor(hex: 0xA371F7))
    static let danger = Color(UIColor(hex: 0xE81123))
}

extension UIColor {
    convenience init(hex: UInt32) {
        self.init(red: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
                  blue: CGFloat(hex & 0xFF) / 255, alpha: 1)
    }
}

extension Font {
    enum GeistWeight: String {
        case regular = "Regular", medium = "Medium", semibold = "SemiBold"
    }

    /// Geist, for everything people read. Scales with Dynamic Type.
    static func geist(_ size: CGFloat, _ weight: GeistWeight = .regular, relativeTo style: TextStyle = .body) -> Font {
        .custom("Geist-\(weight.rawValue)", size: size, relativeTo: style)
    }

    /// Geist Mono, for commands, paths and eyebrows.
    static func mono(_ size: CGFloat, _ weight: GeistWeight = .regular, relativeTo style: TextStyle = .body) -> Font {
        .custom("GeistMono-\(weight == .semibold ? "Medium" : weight.rawValue)", size: size, relativeTo: style)
    }

    static let body16 = geist(16)
    static let callout = geist(15)
    static let small = geist(13, relativeTo: .footnote)
    static let caption12 = geist(12, relativeTo: .caption)
    static let code = mono(12.5, relativeTo: .footnote)
}

/// Corners: chips, buttons and inputs, cards, dialogs.
enum Radius {
    static let chip: CGFloat = 4
    static let control: CGFloat = 8
    static let card: CGFloat = 12
    static let dialog: CGFloat = 16
}

/// One of the brand's line icons (assets/icons), drawn in the color around it.
struct Icon: View {
    let name: String
    var size: CGFloat = 18

    init(_ name: String, size: CGFloat = 18) {
        self.name = name
        self.size = size
    }

    var body: some View {
        Image("Icons/\(name)")
            .resizable()
            .renderingMode(.template)
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}

/// The uppercase mono label over a group: faint, tracked +8%.
struct Eyebrow: View {
    let text: String
    var color: Color = .textFaint

    init(_ text: String, color: Color = .textFaint) {
        self.text = text
        self.color = color
    }

    var body: some View {
        Text(text.uppercased())
            .font(.mono(11, .medium, relativeTo: .caption2))
            .tracking(11 * 0.08)
            .foregroundStyle(color)
    }
}

/// The one primary action of a view: an amber fill with ink text.
struct PrimaryButtonStyle: ButtonStyle {
    /// As tall as an outline button, for a row of them.
    var compact = false
    @Environment(\.isEnabled) private var enabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.geist(compact ? 15 : 16, .semibold))
            .foregroundStyle(Color.onAmber)
            .padding(.horizontal, compact ? 14 : 18)
            .frame(minHeight: compact ? 40 : 48)
            .background(RoundedRectangle(cornerRadius: Radius.control).fill(Color.amber))
            .opacity(enabled ? (configuration.isPressed ? 0.85 : 1) : 0.4)
    }
}

/// Everything else that is a button: an outline on the ground.
struct OutlineButtonStyle: ButtonStyle {
    var tone: Color = .text
    @Environment(\.isEnabled) private var enabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.geist(15, .medium))
            .foregroundStyle(tone)
            .padding(.horizontal, 14)
            .frame(minHeight: 40)
            .background(RoundedRectangle(cornerRadius: Radius.control).fill(configuration.isPressed ? Color.lineStrong : Color.raised))
            .overlay(RoundedRectangle(cornerRadius: Radius.control).strokeBorder(Color.lineStrong))
            .opacity(enabled ? 1 : 0.4)
    }
}

extension ButtonStyle where Self == PrimaryButtonStyle {
    static var primary: PrimaryButtonStyle { PrimaryButtonStyle() }
    static var primaryCompact: PrimaryButtonStyle { PrimaryButtonStyle(compact: true) }
}

extension ButtonStyle where Self == OutlineButtonStyle {
    static var outline: OutlineButtonStyle { OutlineButtonStyle() }
    static func outline(_ tone: Color) -> OutlineButtonStyle { OutlineButtonStyle(tone: tone) }
}

/// A card on the ground: raised, with a hairline.
struct CardBackground: ViewModifier {
    var fill: Color = .raised
    var stroke: Color = .line

    func body(content: Content) -> some View {
        content
            .background(RoundedRectangle(cornerRadius: Radius.card).fill(fill))
            .overlay(RoundedRectangle(cornerRadius: Radius.card).strokeBorder(stroke))
    }
}

extension View {
    func card(fill: Color = .raised, stroke: Color = .line) -> some View {
        modifier(CardBackground(fill: fill, stroke: stroke))
    }
}

/// Puts the brand's type into the system bars, which SwiftUI does not style.
@MainActor
enum Appearance {
    static func apply() {
        let nav = UINavigationBarAppearance()
        nav.configureWithOpaqueBackground()
        nav.backgroundColor = UIColor(Color.ground)
        nav.shadowColor = UIColor(Color.line)
        nav.titleTextAttributes = [
            .font: UIFont(name: "Geist-SemiBold", size: 17) ?? .systemFont(ofSize: 17, weight: .semibold),
            .foregroundColor: UIColor(Color.text),
        ]
        nav.largeTitleTextAttributes = [
            .font: UIFont(name: "Geist-SemiBold", size: 32) ?? .systemFont(ofSize: 32, weight: .semibold),
            .foregroundColor: UIColor(Color.text),
            .kern: -0.96,
        ]
        let back = UIBarButtonItemAppearance()
        back.normal.titleTextAttributes = [.font: UIFont(name: "Geist-Regular", size: 17) ?? .systemFont(ofSize: 17)]
        nav.backButtonAppearance = back
        nav.buttonAppearance = back
        UINavigationBar.appearance().standardAppearance = nav
        UINavigationBar.appearance().scrollEdgeAppearance = nav
        UINavigationBar.appearance().compactAppearance = nav
        UITextField.appearance().tintColor = UIColor(Color.amberInk)
    }
}
