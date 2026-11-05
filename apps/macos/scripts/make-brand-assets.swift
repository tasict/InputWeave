// Draws InputWeave's mark (two screens joined by a zigzag stitch along the
// seam where they meet) and writes, from the one
// geometry below:
//   - the app icon set:      apps/macos/InputWeave/Assets.xcassets/AppIcon.appiconset
//   - the Windows app's icons, all but the executable's (docs/brand/README.md):
//                            apps/windows/InputWeave/Assets/AppIcon.ico, AppIcon.png, AppIcon256.png
//   - the logo for the READMEs and the web: docs/brand/*.svg and docs/brand/app-icon-*.png
//   - the DMG's background:  apps/macos/Installer/dmg/background.tiff
//   - the installer package's background:
//                            apps/macos/Installer/Resources/background.tiff
//
// Run from the repository root:
//   swift apps/macos/scripts/make-brand-assets.swift

import AppKit
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

// MARK: - Brand tokens

struct Palette {
    let thread: String
    let canvas: String
    let grid: String
    let tile: String
    let tileEdge: String
    let ink: String
}

let light = Palette(
    thread: "#8E3FD8", canvas: "#ECEDF1", grid: "#D3D5DC",
    tile: "#FFFFFF", tileEdge: "#C4C7D0", ink: "#1D1D1F")
let dark = Palette(
    thread: "#BF8CFF", canvas: "#18191D", grid: "#2C2E35",
    tile: "#2A2C33", tileEdge: "#41444E", ink: "#F5F5F7")

// MARK: - Geometry, in a 100 × 100 design space with y growing downwards

struct Screen {
    let rect: CGRect
    /// What the screen stands on: a laptop's deck, or a monitor's neck and
    /// foot.
    let stand: [CGRect]
}

/// A laptop on the left and a wider desktop monitor on the right. Their
/// screens share the edge at x = 48, the seam the cursor crosses.
let screens = [
    Screen(
        rect: CGRect(x: 6, y: 33, width: 42, height: 30),
        stand: [CGRect(x: 2.5, y: 63.8, width: 49, height: 2.6)]),
    Screen(
        rect: CGRect(x: 48, y: 22, width: 47, height: 34),
        stand: [CGRect(x: 68, y: 56, width: 7, height: 6.5), CGRect(x: 62.5, y: 62.2, width: 18, height: 2.4)]),
]
let screenCorner: CGFloat = 3
let menuBarHeight: CGFloat = 3

/// The thread: a running stitch that starts on the left screen, goes
/// through the seam and ends at the cursor's tip, the cursor being the
/// needle that pulls it across.
let threadStart = CGPoint(x: 20, y: 54)
let threadControl = CGPoint(x: 44, y: 32)
let cursorTip = CGPoint(x: 67, y: 38)
/// The knot the thread starts from.
let knotRadius: CGFloat = 1.7

/// The macOS arrow pointer with its tip at the origin, in design units.
let cursorOutline: [CGPoint] = [
    (0, 0), (0, 16), (3.9, 12.4), (6.4, 18.2), (9.1, 17.1), (6.6, 11.4), (11.6, 11.4),
].map { CGPoint(x: $0.0 * 0.82, y: $0.1 * 0.82) }

struct Detail {
    /// Draw the thread as stitches; small sizes use one solid line.
    let stitched: Bool
    let threadWidth: CGFloat
    /// The line around each screen; 0 draws none.
    var edgeWidth: CGFloat
    let menuBars: Bool
    let dots: Bool
    let cursor: Bool
    var stands = true

    /// The running stitch's dashes, scaled with the thread so that a
    /// thinner thread keeps the same rhythm.
    var stitch: [CGFloat] { stitchDash.map { $0 * threadWidth / Detail.full.threadWidth } }

    static let full = Detail(stitched: true, threadWidth: 1.9, edgeWidth: 0.9, menuBars: false, dots: true, cursor: true)
    static let small = Detail(stitched: false, threadWidth: 5, edgeWidth: 1.6, menuBars: false, dots: false, cursor: false)
    /// 16 and 32 pixels: only the two screens and a bold thread.
    static let tiny = Detail(
        stitched: false, threadWidth: 7.5, edgeWidth: 2.6, menuBars: false, dots: false, cursor: false,
        stands: false)
}

let stitchDash: [CGFloat] = [3.4, 1.9]
/// Shadow under the screens, in icon pixels at 1024.
let screenShadow = (offset: CGSize(width: 0, height: -8), blur: CGFloat(22), alpha: CGFloat(0.16))

// MARK: - Colors

func cgColor(_ hex: String) -> CGColor {
    let v = Int(hex.dropFirst(), radix: 16)!
    return CGColor(
        srgbRed: CGFloat((v >> 16) & 0xff) / 255, green: CGFloat((v >> 8) & 0xff) / 255,
        blue: CGFloat(v & 0xff) / 255, alpha: 1)
}

// MARK: - Raster drawing

/// Draws the mark into `context`, mapping the 100-unit design space onto
/// `box` (in context coordinates, y up). Shadow offsets ignore the current
/// transform, so `pixelsPerUnit` says how many device pixels one unit of
/// `box` covers, to keep the shadows in proportion.
func drawMark(
    _ context: CGContext, in box: CGRect, palette: Palette, detail: Detail, shadows: Bool, pixelsPerUnit: CGFloat = 1
) {
    context.saveGState()
    context.translateBy(x: box.minX, y: box.maxY)
    context.scaleBy(x: box.width / 100, y: -box.height / 100)

    for screen in screens {
        for part in screen.stand where detail.stands {
            let path = CGPath(roundedRect: part, cornerWidth: 1.2, cornerHeight: 1.2, transform: nil)
            context.addPath(path)
            context.setFillColor(cgColor(palette.tileEdge))
            context.fillPath()
        }
        let path = CGPath(
            roundedRect: screen.rect, cornerWidth: screenCorner, cornerHeight: screenCorner,
            transform: nil)
        context.saveGState()
        if shadows {
            let k = box.width * pixelsPerUnit / 700
            context.setShadow(
                offset: CGSize(width: screenShadow.offset.width * k, height: screenShadow.offset.height * k),
                blur: screenShadow.blur * k, color: CGColor(gray: 0, alpha: screenShadow.alpha))
        }
        context.addPath(path)
        context.setFillColor(cgColor(palette.tile))
        context.fillPath()
        context.restoreGState()
        if detail.menuBars {
            context.saveGState()
            context.addPath(path)
            context.clip()
            context.setFillColor(cgColor(palette.grid))
            context.fill(
                CGRect(x: screen.rect.minX, y: screen.rect.minY, width: screen.rect.width, height: menuBarHeight))
            context.restoreGState()
        }
        if detail.edgeWidth > 0 {
            context.addPath(path)
            context.setStrokeColor(cgColor(palette.tileEdge))
            context.setLineWidth(detail.edgeWidth)
            context.strokePath()
        }
    }

    context.setStrokeColor(cgColor(palette.thread))
    context.setLineWidth(detail.threadWidth)
    context.setLineCap(.round)
    if detail.stitched {
        context.setLineDash(phase: 0, lengths: detail.stitch)
    }
    context.move(to: threadStart)
    let end = detail.cursor ? cursorTip : CGPoint(x: cursorTip.x + 6, y: cursorTip.y - 2)
    context.addQuadCurve(to: end, control: threadControl)
    context.strokePath()
    context.setLineDash(phase: 0, lengths: [])
    if detail.stitched {
        context.setFillColor(cgColor(palette.thread))
        context.fillEllipse(in: CGRect(
            x: threadStart.x - knotRadius, y: threadStart.y - knotRadius,
            width: 2 * knotRadius, height: 2 * knotRadius))
    }

    if detail.cursor {
        let arrow = CGMutablePath()
        arrow.addLines(between: cursorOutline.map { CGPoint(x: $0.x + cursorTip.x, y: $0.y + cursorTip.y) })
        arrow.closeSubpath()
        // A pointer keeps its colors in light and dark mode.
        context.addPath(arrow)
        context.setFillColor(cgColor("#1D1D1F"))
        context.fillPath()
        context.addPath(arrow)
        context.setStrokeColor(CGColor(gray: 1, alpha: 1))
        context.setLineWidth(0.9)
        context.setLineJoin(.round)
        context.strokePath()
    }
    context.restoreGState()
}

/// Where the 100-unit design space goes in the 1024 icon (y up), with the
/// devices' visual center (48.75, 44.2) at the middle of the icon.
func markBox(_ size: CGFloat) -> CGRect {
    CGRect(x: 512 - size * 0.4875, y: 512 - size * (1 - 0.442), width: size, height: size)
}

let dotPitch: CGFloat = 72
let dotRadius: CGFloat = 4
let markSizeFull: CGFloat = 820
/// Small icons have no dots to fill the tile, so the screens grow, but
/// keep a rim of the tile around them.
let markSizeSmall: CGFloat = 850
let markSizeTiny: CGFloat = 860

// MARK: - The app icon

/// The icon's own colors (docs/brand/README.md): the tile is the thread's
/// purple, a little lighter at the top than at the bottom, which makes the
/// screens plain white with no edge, their stands a lighter purple, and the
/// canvas's dot grid white, faint.
let iconTileTop = "#9D56E6"
let iconTileBottom = "#7A2FC8"
let iconDotAlpha: CGFloat = 0.14
let iconPalette = Palette(
    thread: light.thread, canvas: light.thread, grid: "#FFFFFF", tile: "#FFFFFF", tileEdge: "#CBA9EE",
    ink: light.ink)
/// The icon's tile: a rounded square on the 1024 grid.
let iconTile = CGRect(x: 100, y: 100, width: 824, height: 824)
let iconTileCorner: CGFloat = 185

/// What the icon keeps at `pixels` square: everything from 128 pixels, a
/// solid thread and no cursor or dots at 64, only the screens and a bold
/// thread at 32 and below. Never an edge: the screens are white on purple.
func iconDetail(pixels: Int) -> Detail {
    var detail: Detail = pixels <= 32 ? .tiny : pixels <= 64 ? .small : .full
    detail.edgeWidth = 0
    return detail
}

/// The macOS icon at `pixels` square.
func renderIcon(pixels: Int) -> CGImage {
    let context = CGContext(
        data: nil, width: pixels, height: pixels, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    let s = CGFloat(pixels) / 1024
    context.scaleBy(x: s, y: s)
    context.interpolationQuality = .high
    drawIcon(context, detail: iconDetail(pixels: pixels), pixelsPerUnit: s)
    return context.makeImage()!
}

/// Draws the icon into `context` in its 1024-unit space (y up): the tile,
/// its dot grid and the mark, the shadows under the screens sized for
/// `pixelsPerUnit` device pixels per unit. Nothing under the tile: the
/// Dock and the taskbar put no shadow under an icon.
func drawIcon(_ context: CGContext, detail: Detail, pixelsPerUnit: CGFloat) {
    let shape = CGPath(roundedRect: iconTile, cornerWidth: iconTileCorner, cornerHeight: iconTileCorner, transform: nil)
    context.saveGState()
    context.addPath(shape)
    context.clip()
    let gradient = CGGradient(
        colorsSpace: CGColorSpace(name: CGColorSpace.sRGB)!,
        colors: [cgColor(iconTileTop), cgColor(iconTileBottom)] as CFArray, locations: [0, 1])!
    context.drawLinearGradient(
        gradient, start: CGPoint(x: 0, y: iconTile.maxY), end: CGPoint(x: 0, y: iconTile.minY), options: [])
    if detail.dots {
        context.setFillColor(CGColor(gray: 1, alpha: iconDotAlpha))
        var y = iconTile.minY + dotPitch / 2
        while y < iconTile.maxY {
            var x = iconTile.minX + dotPitch / 2
            while x < iconTile.maxX {
                context.fillEllipse(in: CGRect(x: x - dotRadius, y: y - dotRadius, width: 2 * dotRadius, height: 2 * dotRadius))
                x += dotPitch
            }
            y += dotPitch
        }
    }
    context.restoreGState()

    let markSize: CGFloat = detail.dots ? markSizeFull : detail.stands ? markSizeSmall : markSizeTiny
    drawMark(
        context, in: markBox(markSize), palette: iconPalette, detail: detail, shadows: detail.dots,
        pixelsPerUnit: pixelsPerUnit)
}

func pngData(_ image: CGImage) -> Data {
    let data = NSMutableData()
    let destination = CGImageDestinationCreateWithData(data, UTType.png.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(destination, image, nil)
    precondition(CGImageDestinationFinalize(destination), "cannot encode a PNG")
    return data as Data
}

/// Writes a Windows .ico holding these sizes of the icon, each a PNG.
func writeICO(sizes: [Int], to url: URL) {
    let images = sizes.map { pngData(renderIcon(pixels: $0)) }
    var ico = Data()
    func put16(_ v: Int) { ico.append(contentsOf: [UInt8(v & 0xff), UInt8(v >> 8 & 0xff)]) }
    func put32(_ v: Int) { put16(v & 0xffff); put16(v >> 16) }
    put16(0)  // reserved
    put16(1)  // icon
    put16(sizes.count)
    var offset = 6 + 16 * sizes.count
    for (size, image) in zip(sizes, images) {
        let stored = size >= 256 ? 0 : size
        ico.append(contentsOf: [UInt8(stored), UInt8(stored), 0, 0])  // width, height, no palette, reserved
        put16(1)  // planes
        put16(32)  // bits per pixel
        put32(image.count)
        put32(offset)
        offset += image.count
    }
    for image in images {
        ico.append(image)
    }
    try! ico.write(to: url)
}

func writePNG(_ image: CGImage, to url: URL) {
    try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    let destination = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(destination, image, nil)
    precondition(CGImageDestinationFinalize(destination), "cannot write \(url.path)")
}

// MARK: - SVG

func f(_ v: CGFloat) -> String {
    let s = String(format: "%.2f", Double(v))
    return s.replacingOccurrences(of: #"\.?0+$"#, with: "", options: .regularExpression)
}

func markSVG(palette: Palette, detail: Detail) -> String {
    var out = ""
    for screen in screens {
        let r = screen.rect
        let id = "s\(Int(r.minX))"
        for part in screen.stand {
            out += """
                <rect x="\(f(part.minX))" y="\(f(part.minY))" width="\(f(part.width))" height="\(f(part.height))" rx="1.2" fill="\(palette.tileEdge)"/>

                """
        }
        out += """
            <clipPath id="\(id)"><rect x="\(f(r.minX))" y="\(f(r.minY))" width="\(f(r.width))" height="\(f(r.height))" rx="\(f(screenCorner))"/></clipPath>
            <rect x="\(f(r.minX))" y="\(f(r.minY))" width="\(f(r.width))" height="\(f(r.height))" rx="\(f(screenCorner))" fill="\(palette.tile)"/>

            """
        if detail.menuBars {
            out += """
                <rect x="\(f(r.minX))" y="\(f(r.minY))" width="\(f(r.width))" height="\(f(menuBarHeight))" fill="\(palette.grid)" clip-path="url(#\(id))"/>

                """
        }
        if detail.edgeWidth > 0 {
            out += """
                <rect x="\(f(r.minX))" y="\(f(r.minY))" width="\(f(r.width))" height="\(f(r.height))" rx="\(f(screenCorner))" fill="none" stroke="\(palette.tileEdge)" stroke-width="\(f(detail.edgeWidth))"/>

                """
        }
    }
    let dash = detail.stitched ? " stroke-dasharray=\"\(stitchDash.map(f).joined(separator: " "))\"" : ""
    if detail.stitched {
        out += """
            <circle cx="\(f(threadStart.x))" cy="\(f(threadStart.y))" r="\(f(knotRadius))" fill="\(palette.thread)"/>

            """
    }
    out += """
        <path d="M\(f(threadStart.x)) \(f(threadStart.y)) Q\(f(threadControl.x)) \(f(threadControl.y)) \(f(cursorTip.x)) \(f(cursorTip.y))" fill="none" stroke="\(palette.thread)" stroke-width="\(f(detail.threadWidth))" stroke-linecap="round"\(dash)/>

        """
    if detail.cursor {
        let points = cursorOutline.map { "\(f($0.x + cursorTip.x)),\(f($0.y + cursorTip.y))" }.joined(separator: " ")
        out += """
            <polygon points="\(points)" fill="#1D1D1F" stroke="#FFFFFF" stroke-width="0.9" stroke-linejoin="round"/>

            """
    }
    return out
}

/// The mark alone, cropped to the screens.
func markFile(palette: Palette) -> String {
    """
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="1 20 96 48" width="384" height="192" role="img" aria-label="InputWeave">
    \(markSVG(palette: palette, detail: .full))</svg>

    """
}

/// The mark beside the name, set in the system font.
func lockupFile(palette: Palette) -> String {
    """
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="1 20 330 48" width="660" height="96" role="img" aria-label="InputWeave">
    \(markSVG(palette: palette, detail: .full))<text x="104" y="55" font-family="-apple-system, 'SF Pro Display', 'Segoe UI Variable Display', 'Segoe UI', system-ui, sans-serif" font-size="27" font-weight="600" letter-spacing="-0.3" fill="\(palette.ink)">Input<tspan fill="\(palette.thread)">Weave</tspan></text>
    </svg>

    """
}

/// The app icon as SVG, for places that take vectors.
func iconFile() -> String {
    var dots = ""
    var y = iconTile.minY + dotPitch / 2
    while y < iconTile.maxY {
        var x = iconTile.minX + dotPitch / 2
        while x < iconTile.maxX {
            dots += "<circle cx=\"\(f(x))\" cy=\"\(f(y))\" r=\"\(f(dotRadius))\"/>"
            x += dotPitch
        }
        y += dotPitch
    }
    let k = markSizeFull / 100
    let box = markBox(markSizeFull)
    let left = box.minX
    let top = 1024 - box.maxY
    let tile = "x=\"\(f(iconTile.minX))\" y=\"\(f(iconTile.minY))\" width=\"\(f(iconTile.width))\" height=\"\(f(iconTile.height))\" rx=\"\(f(iconTileCorner))\""
    return """
        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024" role="img" aria-label="InputWeave">
        <defs>
        <linearGradient id="tileFill" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="\(iconTileTop)"/><stop offset="1" stop-color="\(iconTileBottom)"/></linearGradient>
        <clipPath id="tileClip"><rect \(tile)/></clipPath>
        </defs>
        <rect \(tile) fill="url(#tileFill)"/>
        <g clip-path="url(#tileClip)" fill="#FFFFFF" fill-opacity="\(f(iconDotAlpha))">\(dots)</g>
        <g transform="translate(\(f(left)) \(f(top))) scale(\(f(k)))">
        \(markSVG(palette: iconPalette, detail: iconDetail(pixels: 1024)))</g>
        </svg>

        """
}

// MARK: - DMG background

/// The DMG's window, in points with y growing downwards: two screens side
/// by side on the canvas, joined by the stitched seam. Finder draws the app
/// on the left screen and the Applications alias on the right one, at the
/// positions in apps/macos/Installer/dmg/layout.applescript.
let dmgSize = CGSize(width: 600, height: 400)
let dmgScreens = [
    CGRect(x: 30, y: 70, width: 270, height: 230),
    CGRect(x: 300, y: 70, width: 270, height: 230),
]
let dmgScreenCorner: CGFloat = 10
let dmgDotPitch: CGFloat = 16
let dmgDotRadius: CGFloat = 1
let dmgThreadWidth: CGFloat = 2.25
let dmgCaption = "Drag InputWeave onto Applications to install."
let dmgCaptionCenterY: CGFloat = 350
/// Light mode's secondary label gray, a touch darker so it reaches 4.5:1
/// on the canvas.
let dmgCaptionColor = "#6A6A6F"

/// The DMG background at `scale` pixels per point, in the light palette:
/// Finder shows the same picture in light and dark mode.
func renderDMGBackground(scale: Int) -> CGImage {
    let palette = light
    let s = CGFloat(scale)
    let context = CGContext(
        data: nil, width: Int(dmgSize.width) * scale, height: Int(dmgSize.height) * scale,
        bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpace(name: CGColorSpace.sRGB)!,
        bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
    context.translateBy(x: 0, y: dmgSize.height * s)
    context.scaleBy(x: s, y: -s)

    context.setFillColor(cgColor(palette.canvas))
    context.fill(CGRect(origin: .zero, size: dmgSize))
    // The arrangement canvas's dot grid.
    context.setFillColor(cgColor(palette.grid))
    var y = dmgDotPitch / 2
    while y < dmgSize.height {
        var x = dmgDotPitch / 2
        while x < dmgSize.width {
            context.fillEllipse(in: CGRect(
                x: x - dmgDotRadius, y: y - dmgDotRadius, width: 2 * dmgDotRadius, height: 2 * dmgDotRadius))
            x += dmgDotPitch
        }
        y += dmgDotPitch
    }

    // The screens, outlined inside their edges like the canvas's tiles.
    for rect in dmgScreens {
        context.addPath(CGPath(
            roundedRect: rect, cornerWidth: dmgScreenCorner, cornerHeight: dmgScreenCorner, transform: nil))
        context.setFillColor(cgColor(palette.tile))
        context.fillPath()
        context.addPath(CGPath(
            roundedRect: rect.insetBy(dx: 0.5, dy: 0.5), cornerWidth: dmgScreenCorner - 0.5,
            cornerHeight: dmgScreenCorner - 0.5, transform: nil))
        context.setStrokeColor(cgColor(palette.tileEdge))
        context.setLineWidth(1)
        context.strokePath()
    }

    // The seam where the screens meet, sewn with the icon's running stitch.
    // The stitches are stretched a little so that whole ones end flush with
    // the straight part of the shared edge.
    let seamX = dmgScreens[0].maxX
    let seamTop = dmgScreens[0].minY + dmgScreenCorner
    let seamBottom = dmgScreens[0].maxY - dmgScreenCorner
    let rhythm = stitchDash.map { $0 * dmgThreadWidth / Detail.full.threadWidth }
    // Round caps add half the width at both ends of every stitch.
    let reach = seamBottom - seamTop - dmgThreadWidth
    let count = ((reach + rhythm[1]) / (rhythm[0] + rhythm[1])).rounded()
    let fit = reach / (count * rhythm[0] + (count - 1) * rhythm[1])
    context.setStrokeColor(cgColor(palette.thread))
    context.setLineWidth(dmgThreadWidth)
    context.setLineCap(.round)
    context.setLineDash(phase: 0, lengths: rhythm.map { $0 * fit })
    context.move(to: CGPoint(x: seamX, y: seamTop + dmgThreadWidth / 2))
    context.addLine(to: CGPoint(x: seamX, y: seamBottom - dmgThreadWidth / 2))
    context.strokePath()
    context.setLineDash(phase: 0, lengths: [])

    let caption = NSAttributedString(
        string: dmgCaption,
        attributes: [
            .font: NSFont.systemFont(ofSize: 13),
            .foregroundColor: NSColor(cgColor: cgColor(dmgCaptionColor))!,
        ])
    let size = caption.size()
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: true)
    caption.draw(at: CGPoint(x: (dmgSize.width - size.width) / 2, y: dmgCaptionCenterY - size.height / 2))
    NSGraphicsContext.restoreGraphicsState()
    return context.makeImage()!
}

/// Writes the 1x and 2x pictures from `render(scale)` into one TIFF, which
/// Finder and Installer read like an image set.
func writeBackground(to url: URL, render: (Int) -> CGImage) {
    try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    let destination = CGImageDestinationCreateWithURL(url as CFURL, UTType.tiff.identifier as CFString, 2, nil)!
    for scale in [1, 2] {
        let dpi = 72 * scale
        let properties: [CFString: Any] = [
            kCGImagePropertyDPIWidth: dpi, kCGImagePropertyDPIHeight: dpi,
            kCGImagePropertyTIFFDictionary: [kCGImagePropertyTIFFCompression: 5],  // LZW
        ]
        CGImageDestinationAddImage(destination, render(scale), properties as CFDictionary)
    }
    precondition(CGImageDestinationFinalize(destination), "cannot write \(url.path)")
}

// MARK: - Installer background

/// Installer draws its background behind the whole pane, under the step
/// list on the left and the text on the right (620 × 416 points on macOS
/// 27), pinned to the bottom left corner by
/// apps/macos/Installer/Distribution.xml. The pane is plain white in light
/// mode and nearly black in dark mode, which is why the mark drawn faintly
/// here before could hardly be seen. The picture is the app icon, the
/// thing the user will recognise in the Dock afterwards, the same in both
/// modes: its tile 128 points wide, 20 points from the pane's left and
/// bottom edges like the window's buttons, so it ends 148 points up and
/// 148 points in; on macOS 27 the step list ends about 245 points above
/// the pane's bottom edge and the text box starts 183 points from its
/// left edge. Everything around the icon is transparent.
let installerTile: CGFloat = 128
let installerMargin: CGFloat = 20
/// The icon's 1024-unit space at the size that makes its tile `installerTile`.
let installerIconSize = installerTile * 1024 / iconTile.width
/// Where that space starts, for the tile's edges to land on the margins.
let installerIconOrigin = installerMargin - installerIconSize * iconTile.minX / 1024
let installerSize = CGSize(
    width: (installerIconOrigin + installerIconSize).rounded(.up),
    height: (installerIconOrigin + installerIconSize).rounded(.up))

/// The installer background at `scale` pixels per point, y up.
func renderInstallerBackground(scale: Int) -> CGImage {
    let context = CGContext(
        data: nil, width: Int(installerSize.width) * scale, height: Int(installerSize.height) * scale,
        bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpace(name: CGColorSpace.sRGB)!,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    context.scaleBy(x: CGFloat(scale), y: CGFloat(scale))
    context.translateBy(x: installerIconOrigin, y: installerIconOrigin)
    let k = installerIconSize / 1024
    context.scaleBy(x: k, y: k)
    context.interpolationQuality = .high
    drawIcon(context, detail: iconDetail(pixels: Int(installerIconSize) * scale), pixelsPerUnit: k * CGFloat(scale))
    return context.makeImage()!
}

// MARK: - Output

let root = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
let iconSet = root.appendingPathComponent("apps/macos/InputWeave/Assets.xcassets/AppIcon.appiconset")
let brand = root.appendingPathComponent("docs/brand")
try FileManager.default.createDirectory(at: brand, withIntermediateDirectories: true)

struct IconSlot {
    let points: Int
    let scale: Int
    var pixels: Int { points * scale }
    var filename: String { "icon_\(points)x\(points)\(scale == 2 ? "@2x" : "").png" }
}

let slots = [16, 32, 128, 256, 512].flatMap { [IconSlot(points: $0, scale: 1), IconSlot(points: $0, scale: 2)] }
for slot in slots {
    writePNG(renderIcon(pixels: slot.pixels), to: iconSet.appendingPathComponent(slot.filename))
}
let images = slots.map {
    """
        {
          "filename" : "\($0.filename)",
          "idiom" : "mac",
          "scale" : "\($0.scale)x",
          "size" : "\($0.points)x\($0.points)"
        }
    """
}.joined(separator: ",\n")
try """
{
  "images" : [
\(images)
  ],
  "info" : {
    "author" : "xcode",
    "version" : 1
  }
}

""".write(to: iconSet.appendingPathComponent("Contents.json"), atomically: true, encoding: .utf8)

writePNG(renderIcon(pixels: 1024), to: brand.appendingPathComponent("app-icon-1024.png"))
writePNG(renderIcon(pixels: 64), to: brand.appendingPathComponent("app-icon-64.png"))
try iconFile().write(to: brand.appendingPathComponent("app-icon.svg"), atomically: true, encoding: .utf8)
// The Windows app's window icon (the sizes the title bar and the taskbar
// ask for), the same at 32 pixels for the title bar, and at 256 for setup
// and notifications. The executable's icon, InputWeave.ico, is made on
// Windows by apps/windows/scripts/MakeAppIcon from the icon set.
let windowsAssets = root.appendingPathComponent("apps/windows/InputWeave/Assets")
writeICO(sizes: [16, 24, 32, 48, 64, 256], to: windowsAssets.appendingPathComponent("AppIcon.ico"))
writePNG(renderIcon(pixels: 32), to: windowsAssets.appendingPathComponent("AppIcon.png"))
writePNG(renderIcon(pixels: 256), to: windowsAssets.appendingPathComponent("AppIcon256.png"))
try markFile(palette: light).write(to: brand.appendingPathComponent("mark.svg"), atomically: true, encoding: .utf8)
try markFile(palette: dark).write(to: brand.appendingPathComponent("mark-dark.svg"), atomically: true, encoding: .utf8)
try lockupFile(palette: light).write(to: brand.appendingPathComponent("logo.svg"), atomically: true, encoding: .utf8)
try lockupFile(palette: dark).write(to: brand.appendingPathComponent("logo-dark.svg"), atomically: true, encoding: .utf8)
writeBackground(to: root.appendingPathComponent("apps/macos/Installer/dmg/background.tiff")) {
    renderDMGBackground(scale: $0)
}
writeBackground(to: root.appendingPathComponent("apps/macos/Installer/Resources/background.tiff")) {
    renderInstallerBackground(scale: $0)
}
print("wrote \(slots.count) icon sizes, the Windows icons, the logo files and the DMG and installer backgrounds")
