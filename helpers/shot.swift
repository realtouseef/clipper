// clipper-shot: one screenshot with ScreenCaptureKit at the display's native pixel
// resolution, leaving out every window of the app that launched it (selection overlay,
// thumbnail cards, control bar).
//
//   clipper-shot --out FILE.png --exclude-pid PID --display-id ID [--rect X,Y,W,H] [--cursor]
//   clipper-shot --out FILE.png --exclude-pid PID --window-id WID [--cursor]
//   clipper-shot --list-windows --exclude-pid PID
//
// --rect is in points relative to the display's top-left. stdout: "ok W H" (pixels) after a
// capture; one JSON array of on-screen windows, front to back, for --list-windows.
// stderr: "error: …" on failure.

import AppKit
import CoreGraphics
import Foundation
import ImageIO
import ScreenCaptureKit
import UniformTypeIdentifiers

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write("error: \(msg)\n".data(using: .utf8)!)
    exit(1)
}

var outPath: String?
var excludePid: pid_t = getppid()
var displayID: CGDirectDisplayID?
var windowID: CGWindowID?
var rect: CGRect?
var cursor = false
var listWindows = false
do {
    var args = CommandLine.arguments.dropFirst().makeIterator()
    while let a = args.next() {
        switch a {
        case "--out": outPath = args.next()
        case "--exclude-pid": excludePid = pid_t(args.next() ?? "") ?? excludePid
        case "--display-id": displayID = CGDirectDisplayID(args.next() ?? "")
        case "--window-id": windowID = CGWindowID(args.next() ?? "")
        case "--rect":
            let v = (args.next() ?? "").split(separator: ",").compactMap { Double($0) }
            if v.count == 4 { rect = CGRect(x: v[0], y: v[1], width: v[2], height: v[3]) }
        case "--cursor": cursor = true
        case "--list-windows": listWindows = true
        default: fail("unknown argument \(a)")
        }
    }
}

if listWindows {
    // CGWindowList is ordered front to back, which is what picking under the pointer needs.
    let info = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
    var out: [[String: Any]] = []
    for w in info {
        guard (w[kCGWindowLayer as String] as? Int) == 0,
              let pid = w[kCGWindowOwnerPID as String] as? Int, pid != Int(excludePid),
              let id = w[kCGWindowNumber as String] as? Int,
              (w[kCGWindowAlpha as String] as? Double ?? 1) > 0,
              let b = w[kCGWindowBounds as String] as? [String: Double],
              let bw = b["Width"], let bh = b["Height"], bw >= 40, bh >= 40
        else { continue }
        out.append([
            "id": id,
            "app": w[kCGWindowOwnerName as String] as? String ?? "",
            "title": w[kCGWindowName as String] as? String ?? "",
            "x": b["X"] ?? 0, "y": b["Y"] ?? 0, "w": bw, "h": bh,
        ])
    }
    let data = try! JSONSerialization.data(withJSONObject: out)
    FileHandle.standardOutput.write(data)
    exit(0)
}

guard let outPath else { fail("--out is required") }
// Single-window filters need a window server connection.
_ = NSApplication.shared

func write(_ image: CGImage, to path: String) {
    let url = URL(fileURLWithPath: path) as CFURL
    guard let dest = CGImageDestinationCreateWithURL(url, UTType.png.identifier as CFString, 1, nil) else { fail("cannot write \(path)") }
    CGImageDestinationAddImage(dest, image, nil)
    guard CGImageDestinationFinalize(dest) else { fail("cannot write \(path)") }
    print("ok \(image.width) \(image.height)")
}

let done = DispatchSemaphore(value: 0)
Task {
    do {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        let config = SCStreamConfiguration()
        config.showsCursor = cursor
        config.captureResolution = .best
        config.colorSpaceName = CGColorSpace.sRGB
        let filter: SCContentFilter
        if let windowID {
            guard let w = content.windows.first(where: { $0.windowID == windowID }) else { fail("window not found") }
            filter = SCContentFilter(desktopIndependentWindow: w)
            let s = CGFloat(filter.pointPixelScale)
            config.width = Int((w.frame.width * s).rounded())
            config.height = Int((w.frame.height * s).rounded())
            config.ignoreShadowsSingleWindow = true
        } else {
            let display = content.displays.first { $0.displayID == displayID } ?? content.displays.first
            guard let display else { fail("no display") }
            let excluded = content.applications.filter { $0.processID == excludePid }
            filter = SCContentFilter(display: display, excludingApplications: excluded, exceptingWindows: [])
            let mode = CGDisplayCopyDisplayMode(display.displayID)
            let scale = Double(mode?.pixelWidth ?? display.width) / Double(max(display.width, 1))
            if let r = rect {
                config.sourceRect = r
                config.width = Int((r.width * scale).rounded())
                config.height = Int((r.height * scale).rounded())
            } else {
                config.width = mode?.pixelWidth ?? display.width
                config.height = mode?.pixelHeight ?? display.height
            }
        }
        let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
        write(image, to: outPath)
    } catch {
        fail(error.localizedDescription)
    }
    done.signal()
}
done.wait()
