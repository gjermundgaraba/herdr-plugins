import Foundation
import IOKit.hid

private let reportID: CFIndex = 6
private let reportSize = 64
private let transportBLE = "Bluetooth Low Energy"
private let outputLock = NSLock()
private var inputBuffer = [UInt8](repeating: 0, count: reportSize)

private func emit(_ value: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: value) else {
        return
    }
    outputLock.lock()
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([0x0A]))
    outputLock.unlock()
}

private func property(_ device: IOHIDDevice, _ key: String) -> String? {
    IOHIDDeviceGetProperty(device, key as CFString) as? String
}

private func openMicro() -> (IOHIDManager, IOHIDDevice, String)? {
    let manager = IOHIDManagerCreate(kCFAllocatorDefault, IOOptionBits(kIOHIDOptionsTypeNone))
    IOHIDManagerSetDeviceMatching(manager, [
        kIOHIDVendorIDKey: 0x303A,
        kIOHIDProductIDKey: 0x8360,
    ] as CFDictionary)
    guard IOHIDManagerOpen(manager, IOOptionBits(kIOHIDOptionsTypeNone)) == kIOReturnSuccess,
          let devices = IOHIDManagerCopyDevices(manager) as? Set<IOHIDDevice> else {
        return nil
    }
    let device = devices.sorted {
        property($0, kIOHIDTransportKey) == "USB"
            && property($1, kIOHIDTransportKey) != "USB"
    }.first
    guard let device,
          IOHIDDeviceOpen(device, IOOptionBits(kIOHIDOptionsTypeNone)) == kIOReturnSuccess else {
        return nil
    }
    return (manager, device, property(device, kIOHIDTransportKey) ?? "unknown")
}

private func inputReport(
    _ context: UnsafeMutableRawPointer?,
    _ result: IOReturn,
    _ sender: UnsafeMutableRawPointer?,
    _ type: IOHIDReportType,
    _ id: UInt32,
    _ report: UnsafeMutablePointer<UInt8>,
    _ length: CFIndex
) {
    emit([
        "type": "data",
        "report": Data(bytes: report, count: length).base64EncodedString(),
    ])
}

private func removed(
    _ context: UnsafeMutableRawPointer?,
    _ result: IOReturn,
    _ sender: UnsafeMutableRawPointer?
) {
    emit(["type": "error", "message": "Codex Micro disconnected"])
    exit(2)
}

guard let (manager, device, transport) = openMicro() else {
    emit(["type": "error", "message": "Codex Micro not found or unavailable"])
    exit(1)
}

IOHIDDeviceRegisterInputReportCallback(
    device,
    &inputBuffer,
    inputBuffer.count,
    inputReport,
    nil
)
IOHIDDeviceRegisterRemovalCallback(device, removed, nil)
IOHIDDeviceScheduleWithRunLoop(
    device,
    CFRunLoopGetMain(),
    CFRunLoopMode.defaultMode.rawValue
)
emit(["type": "ready", "transport": transport])

DispatchQueue.global().async {
    while let line = readLine() {
        guard let data = line.data(using: .utf8),
              let command = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let id = command["id"] as? Int,
              let encoded = command["write"] as? String,
              let report = Data(base64Encoded: encoded),
              report.count == reportSize,
              report.first == UInt8(reportID) else {
            emit(["type": "error", "message": "invalid bridge command"])
            continue
        }
        let wire = transport == transportBLE ? report : report.dropFirst()
        let result = wire.withUnsafeBytes { bytes in
            IOHIDDeviceSetReport(
                device,
                kIOHIDReportTypeOutput,
                reportID,
                bytes.bindMemory(to: UInt8.self).baseAddress!,
                wire.count
            )
        }
        if result == kIOReturnSuccess {
            emit(["type": "wrote", "id": id])
        } else {
            emit([
                "type": "error",
                "id": id,
                "message": String(format: "IOHIDDeviceSetReport failed: 0x%08X", result),
            ])
        }
    }
    exit(0)
}

CFRunLoopRun()
IOHIDDeviceUnscheduleFromRunLoop(
    device,
    CFRunLoopGetMain(),
    CFRunLoopMode.defaultMode.rawValue
)
IOHIDDeviceClose(device, IOOptionBits(kIOHIDOptionsTypeNone))
IOHIDManagerClose(manager, IOOptionBits(kIOHIDOptionsTypeNone))
