// App icon: swift makeicon.swift <out.png>  (1024x1024)
import AppKit

let size: CGFloat = 1024
let img = NSImage(size: NSSize(width: size, height: size))
img.lockFocus()

let rect = NSRect(x: 100, y: 100, width: 824, height: 824)
let path = NSBezierPath(roundedRect: rect, xRadius: 185, yRadius: 185)
NSGradient(colors: [NSColor(red: 1.0, green: 0.62, blue: 0.18, alpha: 1),
                    NSColor(red: 0.93, green: 0.27, blue: 0.36, alpha: 1)])!
    .draw(in: path, angle: -60)

let cfg = NSImage.SymbolConfiguration(pointSize: 470, weight: .bold)
if let bolt = NSImage(systemSymbolName: "bolt.fill", accessibilityDescription: nil)?.withSymbolConfiguration(cfg) {
    let tinted = NSImage(size: bolt.size, flipped: false) { r in
        bolt.draw(in: r)
        NSColor.white.set()
        r.fill(using: .sourceAtop)
        return true
    }
    let s = tinted.size
    tinted.draw(in: NSRect(x: (size - s.width) / 2, y: (size - s.height) / 2 + 10, width: s.width, height: s.height))
}
img.unlockFocus()

let rep = NSBitmapImageRep(data: img.tiffRepresentation!)!
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
