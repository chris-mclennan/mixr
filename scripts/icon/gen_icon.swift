// gen_icon.swift — emit an AppIcon.iconset (all standard sizes) for mixr.
//
//   swift scripts/icon/gen_icon.swift  # writes scripts/icon/AppIcon.iconset
//
// The companion `scripts/icon/build.sh` then runs `iconutil` to
// produce `mixr.app/Contents/Resources/AppIcon.icns`.
//
// Design: a deep-charcoal rounded square with shell-prompt wordmark
// "> mixr" — `>` in mixr's teal accent (DJ-flavored, distinct from
// tmnl's orange and mnml's blue), the name in near-white.

import Foundation
import AppKit

let iconsetDir = URL(fileURLWithPath: CommandLine.arguments.count > 1
    ? CommandLine.arguments[1]
    : "scripts/icon/AppIcon.iconset")

// Second arg "nightly" inverts the palette — accent becomes the
// background, charcoal becomes the wordmark. Same shape so the
// nightly + stable icons read as inverted variants of one design
// at a glance in Cmd+Tab / dock.
let isNightly = CommandLine.arguments.count > 2 && CommandLine.arguments[2] == "nightly"

try? FileManager.default.createDirectory(at: iconsetDir, withIntermediateDirectories: true)

let sizes: [(String, Int)] = [
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
]

func render(_ side: Int) -> Data? {
    let s = CGFloat(side)
    let cs = NSColorSpace.sRGB.cgColorSpace!
    guard let ctx = CGContext(
        data: nil,
        width: side,
        height: side,
        bitsPerComponent: 8,
        bytesPerRow: 0,
        space: cs,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else { return nil }

    // macOS 26 (Tahoe) auto-wraps every app icon in its glass
    // template. Painting full-bleed (no transparent margin) keeps
    // the template wrapping our art directly instead of leaving a
    // weird grey bezel around an inset square.
    let body = CGRect(x: 0, y: 0, width: s, height: s)
    let radius = body.width * 0.22
    let path = CGMutablePath()
    path.addRoundedRect(in: body, cornerWidth: radius, cornerHeight: radius)
    ctx.addPath(path)
    // Stable: charcoal gradient bezel. Nightly: mixr teal gradient,
    // so the two read as inverted variants of one design.
    let topColor: CGColor
    let botColor: CGColor
    if isNightly {
        topColor = CGColor(red: 0.32, green: 0.88, blue: 0.82, alpha: 1.0)
        botColor = CGColor(red: 0.14, green: 0.68, blue: 0.62, alpha: 1.0)
    } else {
        topColor = CGColor(red: 0.18, green: 0.20, blue: 0.24, alpha: 1.0)
        botColor = CGColor(red: 0.10, green: 0.12, blue: 0.14, alpha: 1.0)
    }
    let gradient = CGGradient(
        colorsSpace: cs,
        colors: [topColor, botColor] as CFArray,
        locations: [0, 1]
    )!
    ctx.saveGState()
    ctx.clip()
    ctx.drawLinearGradient(
        gradient,
        start: CGPoint(x: 0, y: s),
        end: CGPoint(x: 0, y: 0),
        options: []
    )
    ctx.restoreGState()

    // Wordmark — bold monospace `mixr` in the app's accent color
    // (teal — DJ-flavored), centered on the charcoal bezel. Kept
    // deliberately simple so the three family icons read as accent-
    // color variants of the same shape.
    let nsCtx = NSGraphicsContext(cgContext: ctx, flipped: false)
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = nsCtx

    // Stable: teal accent on charcoal. Nightly: charcoal on teal.
    let accent = isNightly
        ? NSColor(red: 0.10, green: 0.12, blue: 0.16, alpha: 1.0)
        : NSColor(red: 0.20, green: 0.78, blue: 0.72, alpha: 1.0) // mixr: teal
    let fontSize = s * 0.34
    let font = NSFont.monospacedSystemFont(ofSize: fontSize, weight: .bold)
    let para = NSMutableParagraphStyle()
    para.alignment = .center

    let attributed = NSAttributedString(string: "mixr", attributes: [
        .font: font,
        .foregroundColor: accent,
        .paragraphStyle: para,
        .kern: -fontSize * 0.02,
    ])

    let textSize = attributed.size()
    let textRect = CGRect(
        x: body.minX + (body.width - textSize.width) / 2,
        y: body.midY - textSize.height / 2,
        width: textSize.width,
        height: textSize.height
    )
    attributed.draw(in: textRect)

    NSGraphicsContext.restoreGraphicsState()

    guard let cg = ctx.makeImage() else { return nil }
    let rep = NSBitmapImageRep(cgImage: cg)
    return rep.representation(using: .png, properties: [:])
}

for (name, side) in sizes {
    guard let data = render(side) else {
        FileHandle.standardError.write("render \(side) failed\n".data(using: .utf8)!)
        exit(1)
    }
    let url = iconsetDir.appendingPathComponent(name)
    try data.write(to: url)
    print("wrote \(url.path)")
}
