// Draw an app bundle's icon the way macOS shows it (its Assets.car icon,
// with the system's mask, glass and shadow) into a square PNG:
//
//   render-app-icon <path/to/Some.app> <pixels> <out.png>
//
// Run by scripts/app-icon.sh, which builds a throwaway .app around the
// compiled icon and registers it with Launch Services first (NSWorkspace
// draws an unregistered folder as a plain document).
import AppKit

let args = CommandLine.arguments
guard args.count == 4, let pixels = Int(args[2]), pixels > 0 else {
    FileHandle.standardError.write("usage: render-app-icon <app> <pixels> <out.png>\n".data(using: .utf8)!)
    exit(2)
}
let icon = NSWorkspace.shared.icon(forFile: args[1])
guard
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels, bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
        bytesPerRow: 0, bitsPerPixel: 0)
else { exit(1) }
rep.size = NSSize(width: pixels, height: pixels)
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
icon.draw(
    in: NSRect(x: 0, y: 0, width: pixels, height: pixels), from: .zero, operation: .copy,
    fraction: 1)
NSGraphicsContext.restoreGraphicsState()
guard let png = rep.representation(using: .png, properties: [:]) else { exit(1) }
try png.write(to: URL(fileURLWithPath: args[3]))
