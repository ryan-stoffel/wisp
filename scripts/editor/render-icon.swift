import AppKit

// Renders an SVG into the PNGs of a macOS .iconset directory, keeping transparency.
// Usage: swift scripts/editor/render-icon.swift <icon.svg> <out.iconset>
let args = CommandLine.arguments
guard args.count == 3, let image = NSImage(contentsOf: URL(fileURLWithPath: args[1])) else {
	FileHandle.standardError.write("usage: render-icon.swift <icon.svg> <out.iconset>\n".data(using: .utf8)!)
	exit(2)
}
let iconset = URL(fileURLWithPath: args[2])
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
for points in [16, 32, 128, 256, 512] {
	for scale in [1, 2] {
		let pixels = points * scale
		let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels,
			bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
			colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
		NSGraphicsContext.saveGraphicsState()
		NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
		NSGraphicsContext.current?.imageInterpolation = .high
		image.draw(in: NSRect(x: 0, y: 0, width: pixels, height: pixels), from: .zero, operation: .copy, fraction: 1)
		NSGraphicsContext.restoreGraphicsState()
		let name = scale == 1 ? "icon_\(points)x\(points).png" : "icon_\(points)x\(points)@2x.png"
		try rep.representation(using: .png, properties: [:])!.write(to: iconset.appendingPathComponent(name))
	}
}
