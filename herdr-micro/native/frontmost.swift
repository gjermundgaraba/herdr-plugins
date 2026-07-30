import AppKit
import CoreGraphics
import Foundation

struct Frontmost: Codable {
    let appName: String
    let process: String
    let title: String
}

if CommandLine.arguments.count == 2 &&
    CommandLine.arguments[1] == "post-event-access"
{
    if CGPreflightPostEventAccess() {
        print("granted")
        exit(0)
    }
    fputs("macOS post-event access is denied\n", stderr)
    exit(1)
}

guard let app = NSWorkspace.shared.frontmostApplication else {
    fputs("no frontmost application\n", stderr)
    exit(1)
}

let windows = CGWindowListCopyWindowInfo(
    [.optionOnScreenOnly, .excludeDesktopElements],
    kCGNullWindowID
) as? [[String: Any]] ?? []
let window = windows.first {
    ($0[kCGWindowOwnerPID as String] as? Int32) == app.processIdentifier &&
    ($0[kCGWindowLayer as String] as? Int) == 0
}

if CommandLine.arguments.count == 6 && CommandLine.arguments[1] == "scroll" {
    guard
        let notches = Int32(CommandLine.arguments[2]),
        let x = Double(CommandLine.arguments[3]),
        let y = Double(CommandLine.arguments[4]),
        app.bundleIdentifier == CommandLine.arguments[5],
        let bounds = window?[kCGWindowBounds as String] as? [String: CGFloat],
        let originX = bounds["X"],
        let originY = bounds["Y"],
        let width = bounds["Width"],
        let height = bounds["Height"]
    else {
        fputs("frontmost scroll target unavailable\n", stderr)
        exit(1)
    }
    let location = CGPoint(
        x: originX + width * x,
        y: originY + height * y
    )
    let originalLocation = CGEvent(source: nil)?.location
    CGWarpMouseCursorPosition(location)
    CGEvent(
        mouseEventSource: nil,
        mouseType: .mouseMoved,
        mouseCursorPosition: location,
        mouseButton: .left
    )?.post(tap: .cghidEventTap)
    usleep(50_000)
    defer {
        if let originalLocation {
            CGWarpMouseCursorPosition(originalLocation)
            CGEvent(
                mouseEventSource: nil,
                mouseType: .mouseMoved,
                mouseCursorPosition: originalLocation,
                mouseButton: .left
            )?.post(tap: .cghidEventTap)
        }
    }
    for _ in 0..<abs(notches) {
        guard let event = CGEvent(
            scrollWheelEvent2Source: nil,
            units: .line,
            wheelCount: 1,
            wheel1: notches > 0 ? 1 : -1,
            wheel2: 0,
            wheel3: 0
        ) else {
            fputs("could not create scroll event\n", stderr)
            exit(1)
        }
        event.location = location
        event.post(tap: .cghidEventTap)
    }
    usleep(50_000)
    exit(0)
}

let frontmost = Frontmost(
    appName: app.localizedName ?? "",
    process: app.bundleIdentifier ?? "",
    title: window?[kCGWindowName as String] as? String ?? ""
)

FileHandle.standardOutput.write(try JSONEncoder().encode(frontmost))
