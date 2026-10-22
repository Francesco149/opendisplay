// Fake OpenDisplay sender — the counterpart of fake-receiver.swift: speaks
// just enough of the wire protocol to drive a receiver without a Mac
// desktop, Screen Recording permission or a virtual display. It reads the
// receiver's hello, encodes a moving test pattern with VideoToolbox H.264 at
// the size and rate the hello allows (PROTOCOL.md 6.5), answers pings and
// keyframe requests, and prints the receiver's own `stats` reports, which
// say whether frames really arrived and how fast.
//
//   swiftc -O tools/fake-sender.swift -o build/fake-sender
//   build/fake-sender --usb [UDID]        through usbmuxd to an iPhone/iPad
//   build/fake-sender --host 127.0.0.1    TCP (simulator, WiFi receiver)
//   options: --port 9000 --seconds 30 --fps 60 --size 1600x1200
//
// Exits 0 when the receiver reported a nonzero frame rate, 1 otherwise.
import CoreGraphics
import CoreText
import CoreVideo
import Foundation
import VideoToolbox

// MARK: - Options

var usbUDID: String?
var useUSB = false
var host = "127.0.0.1"
var port: UInt16 = 9000
var seconds = 30.0
var fpsOverride: Int?
var sizeOverride: (Int, Int)?
var args = Array(CommandLine.arguments.dropFirst())
while !args.isEmpty {
    let arg = args.removeFirst()
    switch arg {
    case "--usb":
        useUSB = true
        if let next = args.first, !next.hasPrefix("--") { usbUDID = args.removeFirst() }
    case "--host": host = args.removeFirst()
    case "--port": port = UInt16(args.removeFirst())!
    case "--seconds": seconds = Double(args.removeFirst())!
    case "--fps": fpsOverride = Int(args.removeFirst())!
    case "--size":
        let parts = args.removeFirst().split(separator: "x").compactMap { Int($0) }
        sizeOverride = (parts[0], parts[1])
    default:
        FileHandle.standardError.write(Data("unknown option \(arg)\n".utf8))
        exit(2)
    }
}

func log(_ message: String) {
    let stamp = String(format: "%7.2f", Date().timeIntervalSince(startTime))
    print("[\(stamp)] \(message)")
    fflush(stdout)
}
let startTime = Date()
var nowMs: Double { Date().timeIntervalSince1970 * 1000 }

// MARK: - Sockets

func readExactly(_ fd: Int32, _ count: Int) -> Data? {
    var data = Data(count: count)
    var got = 0
    while got < count {
        let n = data.withUnsafeMutableBytes { read(fd, $0.baseAddress! + got, count - got) }
        if n <= 0 { return nil }
        got += n
    }
    return data
}

func writeAll(_ fd: Int32, _ data: Data) -> Bool {
    data.withUnsafeBytes { raw in
        var sent = 0
        while sent < raw.count {
            let n = write(fd, raw.baseAddress! + sent, raw.count - sent)
            if n <= 0 { return false }
            sent += n
        }
        return true
    }
}

/// usbmuxd: [len LE][version 1][type 8 = plist][tag] + XML plist.
func usbmuxRequest(_ fd: Int32, _ message: [String: Any]) -> [String: Any]? {
    var body = message
    body["ClientVersionString"] = "fake-sender"
    body["ProgName"] = "fake-sender"
    body["kLibUSBMuxVersion"] = 3
    let plist = try! PropertyListSerialization.data(fromPropertyList: body, format: .xml, options: 0)
    var header = [UInt32(16 + plist.count), 1, 8, 1].map { $0.littleEndian }
    var packet = Data(bytes: &header, count: 16)
    packet.append(plist)
    guard writeAll(fd, packet), let head = readExactly(fd, 16) else { return nil }
    let length = head.withUnsafeBytes { Int(UInt32(littleEndian: $0.load(as: UInt32.self))) }
    guard let reply = readExactly(fd, length - 16) else { return nil }
    return try? PropertyListSerialization.propertyList(from: reply, format: nil) as? [String: Any]
}

func unixSocket(_ path: String) -> Int32 {
    let fd = socket(AF_UNIX, SOCK_STREAM, 0)
    var addr = sockaddr_un()
    addr.sun_family = sa_family_t(AF_UNIX)
    withUnsafeMutableBytes(of: &addr.sun_path) { raw in
        _ = path.withCString { strncpy(raw.baseAddress!.assumingMemoryBound(to: CChar.self), $0, raw.count - 1) }
    }
    let ok = withUnsafePointer(to: &addr) {
        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
        }
    }
    precondition(ok == 0, "cannot reach \(path)")
    return fd
}

func connectUSB() -> Int32 {
    let list = unixSocket("/var/run/usbmuxd")
    guard let reply = usbmuxRequest(list, ["MessageType": "ListDevices"]),
          let devices = reply["DeviceList"] as? [[String: Any]] else { fatalError("usbmuxd ListDevices failed") }
    close(list)
    let usb = devices.filter { ($0["Properties"] as? [String: Any])?["ConnectionType"] as? String == "USB" }
    guard let device = usb.first(where: {
        usbUDID == nil || ($0["Properties"] as? [String: Any])?["SerialNumber"] as? String == usbUDID
    }), let id = device["DeviceID"] as? Int else {
        fatalError("no USB device\(usbUDID.map { " \($0)" } ?? "") in usbmuxd")
    }
    let serial = (device["Properties"] as? [String: Any])?["SerialNumber"] as? String ?? "?"
    let fd = unixSocket("/var/run/usbmuxd")
    let reply2 = usbmuxRequest(fd, ["MessageType": "Connect", "DeviceID": id,
                                    "PortNumber": Int((port << 8) | (port >> 8))])
    guard reply2?["Number"] as? Int == 0 else {
        fatalError("usbmuxd Connect to \(serial):\(port) failed: \(reply2 ?? [:]) (is the app open?)")
    }
    log("connected to \(serial):\(port) over USB")
    return fd
}

func connectTCP() -> Int32 {
    var hints = addrinfo(ai_flags: 0, ai_family: AF_UNSPEC, ai_socktype: SOCK_STREAM,
                         ai_protocol: 0, ai_addrlen: 0, ai_canonname: nil, ai_addr: nil, ai_next: nil)
    var result: UnsafeMutablePointer<addrinfo>?
    precondition(getaddrinfo(host, String(port), &hints, &result) == 0, "cannot resolve \(host)")
    let ai = result!.pointee
    let fd = socket(ai.ai_family, ai.ai_socktype, ai.ai_protocol)
    guard connect(fd, ai.ai_addr, ai.ai_addrlen) == 0 else { fatalError("connect \(host):\(port) failed") }
    freeaddrinfo(result)
    var one: Int32 = 1
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, socklen_t(MemoryLayout<Int32>.size))
    log("connected to \(host):\(port) over TCP")
    return fd
}

let fd = useUSB ? connectUSB() : connectTCP()
let sendLock = NSLock()

func sendFrame(_ payload: Data) -> Bool {
    var header = UInt32(payload.count).bigEndian
    var frame = Data(bytes: &header, count: 4)
    frame.append(payload)
    sendLock.lock(); defer { sendLock.unlock() }
    return writeAll(fd, frame)
}

func sendJSON(_ object: [String: Any]) {
    _ = sendFrame(try! JSONSerialization.data(withJSONObject: object))
}

// MARK: - Encoder

final class Encoder {
    let width: Int, height: Int, fps: Int
    private var session: VTCompressionSession?
    private var frameIndex: Int64 = 0
    var forceKeyframe = true
    private(set) var framesSent = 0
    private(set) var bytesSent = 0

    init(width: Int, height: Int, fps: Int) {
        self.width = width; self.height = height; self.fps = fps
        let status = VTCompressionSessionCreate(
            allocator: nil, width: Int32(width), height: Int32(height),
            codecType: kCMVideoCodecType_H264, encoderSpecification: nil,
            imageBufferAttributes: nil, compressedDataAllocator: nil,
            outputCallback: nil, refcon: nil, compressionSessionOut: &session)
        precondition(status == noErr, "VTCompressionSessionCreate \(status)")
        let s = session!
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ProfileLevel,
                             value: kVTProfileLevel_H264_High_AutoLevel)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ExpectedFrameRate, value: fps as CFNumber)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate,
                             value: 12_000_000 as CFNumber)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_MaxKeyFrameInterval, value: fps * 10 as CFNumber)
        VTCompressionSessionPrepareToEncodeFrames(s)
    }

    deinit { if let session { VTCompressionSessionInvalidate(session) } }

    func encode() {
        guard let session else { return }
        let capture = nowMs
        let buffer = TestPattern.render(width: width, height: height, frame: Int(frameIndex))
        let pts = CMTime(value: frameIndex, timescale: CMTimeScale(fps))
        frameIndex += 1
        var props: CFDictionary?
        if forceKeyframe {
            forceKeyframe = false
            props = [kVTEncodeFrameOptionKey_ForceKeyFrame: kCFBooleanTrue] as CFDictionary
        }
        VTCompressionSessionEncodeFrame(session, imageBuffer: buffer, presentationTimeStamp: pts,
                                        duration: .invalid, frameProperties: props,
                                        infoFlagsOut: nil) { [weak self] status, _, sample in
            guard status == noErr, let sample, let self else { return }
            self.ship(sample, captureMs: capture)
        }
    }

    /// AVCC -> Annex B with 4-byte start codes, SPS/PPS ahead of every IDR,
    /// telemetry prefix first (PROTOCOL.md 5.1).
    private func ship(_ sample: CMSampleBuffer, captureMs: Double) {
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false)
            as? [[CFString: Any]]
        let isKey = !(attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool ?? false)
        let startCode = Data([0, 0, 0, 1])
        var payload = Data()
        if isKey, let format = CMSampleBufferGetFormatDescription(sample) {
            for index in 0..<2 {
                var pointer: UnsafePointer<UInt8>?
                var size = 0
                if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    format, parameterSetIndex: index, parameterSetPointerOut: &pointer,
                    parameterSetSizeOut: &size, parameterSetCountOut: nil,
                    nalUnitHeaderLengthOut: nil) == noErr, let pointer {
                    payload.append(startCode)
                    payload.append(pointer, count: size)
                }
            }
        }
        guard let block = CMSampleBufferGetDataBuffer(sample) else { return }
        var length = 0
        var base: UnsafeMutablePointer<Int8>?
        CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: nil,
                                    totalLengthOut: &length, dataPointerOut: &base)
        guard let base else { return }
        var offset = 0
        while offset + 4 <= length {
            let naluLength = (0..<4).reduce(0) { ($0 << 8) | Int(UInt8(bitPattern: base[offset + $1])) }
            payload.append(startCode)
            payload.append(UnsafeRawPointer(base + offset + 4).assumingMemoryBound(to: UInt8.self),
                           count: naluLength)
            offset += 4 + naluLength
        }
        var frame = Data("{\"cap\":\(captureMs),\"snd\":\(nowMs)}".utf8)
        frame.append(payload)
        if sendFrame(frame) {
            framesSent += 1
            bytesSent += frame.count
        }
    }
}

enum TestPattern {
    static func render(width: Int, height: Int, frame: Int) -> CVPixelBuffer {
        var buffer: CVPixelBuffer?
        CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_32BGRA,
                            [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary, &buffer)
        let pixels = buffer!
        CVPixelBufferLockBaseAddress(pixels, [])
        defer { CVPixelBufferUnlockBaseAddress(pixels, []) }
        let ctx = CGContext(data: CVPixelBufferGetBaseAddress(pixels), width: width, height: height,
                            bitsPerComponent: 8, bytesPerRow: CVPixelBufferGetBytesPerRow(pixels),
                            space: CGColorSpaceCreateDeviceRGB(),
                            bitmapInfo: CGImageAlphaInfo.noneSkipFirst.rawValue
                                | CGBitmapInfo.byteOrder32Little.rawValue)!
        // SMPTE-ish bars, a sweeping block, and the frame number.
        let bars: [(CGFloat, CGFloat, CGFloat)] = [(0.75, 0.75, 0.75), (0.75, 0.75, 0), (0, 0.75, 0.75),
                                                   (0, 0.75, 0), (0.75, 0, 0.75), (0.75, 0, 0), (0, 0, 0.75)]
        let barWidth = CGFloat(width) / CGFloat(bars.count)
        for (i, c) in bars.enumerated() {
            ctx.setFillColor(red: c.0, green: c.1, blue: c.2, alpha: 1)
            ctx.fill(CGRect(x: CGFloat(i) * barWidth, y: 0, width: barWidth + 1, height: CGFloat(height)))
        }
        let block = CGFloat(height) / 6
        let travel = CGFloat(width) - block
        let phase = CGFloat(frame % 240) / 240
        ctx.setFillColor(red: 1, green: 1, blue: 1, alpha: 1)
        ctx.fill(CGRect(x: travel * phase, y: CGFloat(height) / 2 - block / 2, width: block, height: block))
        let text = "OpenDisplay fake sender  \(width)x\(height)  frame \(frame)" as CFString
        let font = CTFontCreateWithName("Menlo-Bold" as CFString, CGFloat(min(width, height)) / 32, nil)
        let attributes = [kCTFontAttributeName: font,
                          kCTForegroundColorAttributeName: CGColor(gray: 0, alpha: 1)] as CFDictionary
        let line = CTLineCreateWithAttributedString(CFAttributedStringCreate(nil, text, attributes)!)
        ctx.setFillColor(red: 1, green: 1, blue: 1, alpha: 1)
        ctx.fill(CGRect(x: 0, y: CGFloat(height) * 0.08, width: CGFloat(width), height: CGFloat(height) / 12))
        ctx.textPosition = CGPoint(x: CGFloat(width) * 0.03, y: CGFloat(height) * 0.1)
        CTLineDraw(line, ctx)
        return pixels
    }
}

// MARK: - Session

let stateLock = NSLock()
var encoder: Encoder?
var reportedFps: [Int] = []

/// Stream size and rate from hello: the panel, inside every H.264 limit it
/// announces (videoCaps and the legacy maxEncode ceiling), aspect kept.
func configure(from hello: [String: Any]) -> (Int, Int, Int) {
    var w = hello["pixelsWide"] as? Int ?? 1920
    var h = hello["pixelsHigh"] as? Int ?? 1080
    var fps = 60
    let caps = (hello["videoCaps"] as? [[String: Any]])?.first { $0["codec"] as? String == "h264" }
    let maxW = caps?["maxWidth"] as? Int ?? hello["maxEncodeWide"] as? Int
    let maxH = caps?["maxHeight"] as? Int ?? hello["maxEncodeHigh"] as? Int
    if let maxW, let maxH, w > maxW || h > maxH {
        let scale = min(Double(maxW) / Double(w), Double(maxH) / Double(h))
        w = Int(Double(w) * scale) & ~1
        h = Int(Double(h) * scale) & ~1
    }
    if let (ow, oh) = sizeOverride { (w, h) = (ow, oh) }
    if let rate = caps?["maxFrameRate"] as? Int { fps = min(fps, rate) }
    if let budget = caps?["maxPixelsPerSecond"] as? Int { fps = max(1, min(fps, budget / (w * h))) }
    if let fpsOverride { fps = fpsOverride }
    return (w, h, fps)
}

func handleControl(_ object: [String: Any]) {
    switch object["type"] as? String {
    case "hello":
        let (w, h, fps) = configure(from: object)
        log("hello: device=\(object["device"] ?? "?") panel=\(object["pixelsWide"] ?? 0)x\(object["pixelsHigh"] ?? 0) "
            + "pv=\(object["pv"] ?? "-") videoCaps=\(object["videoCaps"] ?? "-") -> streaming \(w)x\(h)@\(fps)")
        stateLock.lock()
        let reuse = encoder.map { $0.width == w && $0.height == h && $0.fps == fps } ?? false
        if !reuse {
            sendJSON(["type": "welcome", "pv": 3, "min": 1])
            sendJSON(["type": "streamConfig", "codec": "h264", "width": w, "height": h, "framesPerSecond": fps])
            encoder = Encoder(width: w, height: h, fps: fps)
        }
        stateLock.unlock()
    case "ping":
        sendJSON(["type": "pong", "t": object["t"] ?? 0, "mt": nowMs])
    case "kf":
        log("receiver asked for a keyframe")
        stateLock.lock(); encoder?.forceKeyframe = true; stateLock.unlock()
    case "stats":
        let fps = object["fps"] as? Int ?? 0
        reportedFps.append(fps)
        log("receiver stats: fps=\(fps) mbps=\(object["mbps"] ?? 0) e2e50=\(object["e2e50"] ?? 0) "
            + "e2e95=\(object["e2e95"] ?? 0) stalls=\(object["stalls"] ?? 0) "
            + "transport=\(object["transport"] ?? "?") codec=\(object["codec"] ?? "?")")
    case let type?:
        if !["touch", "scroll"].contains(type) { log("receiver: \(type) \(object)") }
    default:
        break
    }
}

Thread.detachNewThread {
    while let head = readExactly(fd, 4) {
        let length = head.withUnsafeBytes { Int(UInt32(bigEndian: $0.load(as: UInt32.self))) }
        guard length > 0, length < 1 << 20, let payload = readExactly(fd, length) else { break }
        if let object = try? JSONSerialization.jsonObject(with: payload) as? [String: Any] {
            handleControl(object)
        }
    }
    log("receiver closed the connection")
    exit(reportedFps.contains { $0 > 0 } ? 0 : 1)
}

// Frame clock + liveness pings until the deadline. Frames are due on a fixed
// grid (not "sleep one interval after each encode"), so the sender really
// offers the negotiated rate and the receiver's fps measures its decoder.
var lastPing = Date.distantPast
var lastReport = Date()
var nextFrame = Date()
while Date().timeIntervalSince(startTime) < seconds {
    stateLock.lock()
    let current = encoder
    stateLock.unlock()
    if let current {
        current.encode()
        if Date().timeIntervalSince(lastReport) >= 5 {
            log("sent \(current.framesSent) frames, \(current.bytesSent / 1024) KiB")
            lastReport = Date()
        }
    }
    if Date().timeIntervalSince(lastPing) >= 2 {
        sendJSON(["type": "ping", "encDrops": 0, "netDrops": 0])
        lastPing = Date()
    }
    nextFrame += 1.0 / Double(current?.fps ?? 10)
    let wait = nextFrame.timeIntervalSinceNow
    if wait > 0 { Thread.sleep(forTimeInterval: wait) } else if wait < -0.5 { nextFrame = Date() }
}
let ok = reportedFps.contains { $0 > 0 }
log(ok ? "PASS: receiver reported fps \(reportedFps)" : "FAIL: receiver never reported a frame rate")
close(fd)
exit(ok ? 0 : 1)
