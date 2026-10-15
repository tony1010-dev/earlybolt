// Tray icons: swift maketray.swift <out dir>
// macOS: monochrome templates (the menu bar tints them) / Windows: color
import AppKit

func render(_ px: CGFloat, _ draw: (CGFloat) -> Void, to path: String) {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(px), pixelsHigh: Int(px), bitsPerSample: 8,
                               samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                               bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    draw(px)
    NSGraphicsContext.restoreGraphicsState()
    try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: path))
}

func symbol(_ name: String, _ pt: CGFloat, _ color: NSColor) -> NSImage {
    let cfg = NSImage.SymbolConfiguration(pointSize: pt, weight: .bold)
    let img = NSImage(systemSymbolName: name, accessibilityDescription: nil)!.withSymbolConfiguration(cfg)!
    return NSImage(size: img.size, flipped: false) { r in
        img.draw(in: r); color.set(); r.fill(using: .sourceAtop); return true
    }
}

func drawCentered(_ img: NSImage, in px: CGFloat) {
    let s = img.size
    let scale = min(px * 0.86 / s.width, px * 0.86 / s.height)
    let w = s.width * scale, h = s.height * scale
    img.draw(in: NSRect(x: (px - w) / 2, y: (px - h) / 2, width: w, height: h))
}

let out = CommandLine.arguments[1]
let symbols = ["on": "bolt.fill", "off": "bolt.slash.fill", "warn": "exclamationmark.triangle.fill"]
for (state, name) in symbols {
    render(44, { px in drawCentered(symbol(name, 40, .black), in: px) }, to: "\(out)/tray-mac-\(state).png")
    let bg: NSColor = state == "on" ? NSColor(red: 0.98, green: 0.45, blue: 0.22, alpha: 1)
                    : state == "warn" ? NSColor(red: 0.86, green: 0.2, blue: 0.24, alpha: 1)
                    : NSColor(white: 0.55, alpha: 1)
    render(32, { px in
        bg.set()
        NSBezierPath(roundedRect: NSRect(x: 1, y: 1, width: px - 2, height: px - 2), xRadius: 7, yRadius: 7).fill()
        let inner = symbol(name, 40, .white)
        let s = inner.size, scale = min(px * 0.62 / s.width, px * 0.62 / s.height)
        let w = s.width * scale, h = s.height * scale
        inner.draw(in: NSRect(x: (px - w) / 2, y: (px - h) / 2, width: w, height: h))
    }, to: "\(out)/tray-win-\(state).png")
}
