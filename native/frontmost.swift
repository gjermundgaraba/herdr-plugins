import AppKit
import CoreGraphics
import Foundation

struct Frontmost: Codable {
    let appName: String
    let process: String
    let title: String
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
let frontmost = Frontmost(
    appName: app.localizedName ?? "",
    process: app.bundleIdentifier ?? "",
    title: window?[kCGWindowName as String] as? String ?? ""
)

FileHandle.standardOutput.write(try JSONEncoder().encode(frontmost))
