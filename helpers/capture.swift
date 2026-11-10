// clipper-capture: records one display (and optionally a microphone) into an MP4 with
// ScreenCaptureKit, leaving out every window of the app that launched it (the recording
// control bar), which AVFoundation's screen input can't do.
//
//   clipper-capture --out FILE --exclude-pid PID [--display N | --display-id ID]
//                   [--rect X,Y,W,H] [--fps 30] [--mic NAME|default] [--muted]
//
// --rect records only that region of the display (in points, relative to its top-left);
// ScreenCaptureKit crops at capture, so nothing outside it is ever stored.
//
// stdin:  "q" stops and finalizes the file; "m1" / "m0" mutes / unmutes the mic (silence is
//         still recorded, so audio and video stay aligned).
// stderr: "  Duration: N/A, start: <host seconds>, bitrate: N/A" once the first frame is in
//         (file time t = host time start + t; the same line ffmpeg prints), "frame=1" with it,
//         "nomic" if the microphone couldn't be opened, "error: …" on failure.
//
// Timestamps are on the host clock (mach_absolute_time), the clock the cursor tracker and the
// webcam capture use, so all tracks line up.

import AVFoundation
import CoreMedia
import Foundation
import ScreenCaptureKit

setvbuf(stderr, nil, _IONBF, 0)

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write("error: \(msg)\n".data(using: .utf8)!)
    exit(1)
}

func say(_ msg: String) {
    FileHandle.standardError.write("\(msg)\n".data(using: .utf8)!)
}

// ---------- arguments ----------

var outPath: String?
var excludePid: pid_t = getppid()
var displayIndex = 0
var displayID: CGDirectDisplayID?
var rect: CGRect?
var fps = 30
var micName: String?
var startMuted = false
do {
    var args = CommandLine.arguments.dropFirst().makeIterator()
    while let a = args.next() {
        switch a {
        case "--out": outPath = args.next()
        case "--exclude-pid": excludePid = pid_t(args.next() ?? "") ?? excludePid
        case "--display": displayIndex = Int(args.next() ?? "") ?? 0
        case "--display-id": displayID = CGDirectDisplayID(args.next() ?? "")
        case "--rect":
            let v = (args.next() ?? "").split(separator: ",").compactMap { Double($0) }
            if v.count == 4 { rect = CGRect(x: v[0], y: v[1], width: v[2], height: v[3]) }
        case "--fps": fps = Int(args.next() ?? "") ?? 30
        case "--mic": micName = args.next()
        case "--muted": startMuted = true
        default: fail("unknown argument \(a)")
        }
    }
}
guard let outPath else { fail("--out is required") }

// ---------- writer ----------

final class Recorder: NSObject, SCStreamOutput, SCStreamDelegate {
    let writer: AVAssetWriter
    let video: AVAssetWriterInput
    var audio: AVAssetWriterInput?
    let queue = DispatchQueue(label: "capture")
    var started = false
    var firstPTS = CMTime.invalid
    var lastVideo: CMSampleBuffer?
    var lastPTS = CMTime.invalid
    var muted = false
    var finished = false
    var fps = 30
    var timer: DispatchSourceTimer?

    init(url: URL, width: Int, height: Int, fps: Int, mic: Bool) throws {
        try? FileManager.default.removeItem(at: url)
        writer = try AVAssetWriter(outputURL: url, fileType: .mp4)
        video = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: width,
            AVVideoHeightKey: height,
            AVVideoCompressionPropertiesKey: [
                AVVideoAverageBitRateKey: 24_000_000,
                AVVideoExpectedSourceFrameRateKey: fps,
                // A keyframe every second, so the editor can seek to any frame quickly.
                AVVideoMaxKeyFrameIntervalKey: fps,
                AVVideoMaxKeyFrameIntervalDurationKey: 1.0,
                AVVideoProfileLevelKey: AVVideoProfileLevelH264HighAutoLevel,
            ],
        ])
        video.expectsMediaDataInRealTime = true
        writer.add(video)
        if mic {
            let a = AVAssetWriterInput(mediaType: .audio, outputSettings: [
                AVFormatIDKey: kAudioFormatMPEG4AAC,
                AVSampleRateKey: 48_000,
                AVNumberOfChannelsKey: 1,
                AVEncoderBitRateKey: 160_000,
            ])
            a.expectsMediaDataInRealTime = true
            writer.add(a)
            audio = a
        }
        super.init()
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard !finished, sb.isValid else { return }
        switch type {
        case .screen:
            // Only frames with new content carry an image.
            guard
                let info = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
                let raw = info.first?[.status] as? Int,
                SCFrameStatus(rawValue: raw) == .complete
            else { return }
            let pts = CMSampleBufferGetPresentationTimeStamp(sb)
            if !started {
                guard writer.startWriting() else { fail("could not start writing: \(writer.error?.localizedDescription ?? "?")") }
                writer.startSession(atSourceTime: pts)
                started = true
                firstPTS = pts
                say(String(format: "  Duration: N/A, start: %.6f, bitrate: N/A", CMTimeGetSeconds(pts)))
                say("frame=1")
            }
            // A frame that arrives late can be older than a repeat already written; the
            // writer fails for good on out-of-order times, so drop it.
            if lastPTS.isValid, CMTimeCompare(pts, lastPTS) <= 0 { return }
            // Repeat the previous frame up to this one, one frame apart.
            let half = CMTime(value: 1, timescale: CMTimeScale(fps * 2))
            fill(upTo: CMTimeSubtract(pts, half))
            if video.isReadyForMoreMediaData {
                append(sb, pts)
                lastVideo = sb
            }
        case .microphone:
            guard started, let audio, audio.isReadyForMoreMediaData else { return }
            guard CMTimeCompare(CMSampleBufferGetPresentationTimeStamp(sb), firstPTS) >= 0 else { return }
            if muted { silence(sb) }
            audio.append(sb)
        default:
            break
        }
    }

    /// ScreenCaptureKit only sends frames when the screen changes. Repeat the last frame
    /// while the screen is still, so the file keeps a steady frame rate.
    func startRepeating() {
        let t = DispatchSource.makeTimerSource(queue: queue)
        let interval = 1.0 / Double(fps)
        t.schedule(deadline: .now() + interval, repeating: interval)
        t.setEventHandler { [weak self] in self?.repeatLast() }
        t.resume()
        timer = t
    }

    func repeatLast() {
        guard started, !finished else { return }
        // Short gaps are filled when the next frame arrives. While the screen stays still,
        // fill up to a few frames before now: the margin lets a real frame that's still on
        // its way (they arrive late under load) land after the repeats, in order; the
        // writer fails for good on out-of-order times.
        let interval = CMTime(value: 1, timescale: CMTimeScale(fps))
        let now = CMClockGetTime(CMClockGetHostTimeClock())
        fill(upTo: CMTimeSubtract(now, CMTimeMultiply(interval, multiplier: 5)))
    }

    /// Repeats the last frame at one-frame steps while those times are at or before `end`.
    func fill(upTo end: CMTime) {
        let interval = CMTime(value: 1, timescale: CMTimeScale(fps))
        var n = 0
        while let last = lastVideo, n < 4 * fps, video.isReadyForMoreMediaData {
            let pts = CMTimeAdd(lastPTS, interval)
            guard CMTimeCompare(pts, end) <= 0, let copy = retimed(last, pts) else { break }
            append(copy, pts)
            n += 1
        }
    }

    func append(_ sb: CMSampleBuffer, _ pts: CMTime) {
        if !video.append(sb) || writer.status == .failed {
            fail("could not write the video: \(writer.error?.localizedDescription ?? "unknown error")")
        }
        lastPTS = pts
    }

    func retimed(_ sb: CMSampleBuffer, _ pts: CMTime) -> CMSampleBuffer? {
        var timing = CMSampleTimingInfo(duration: .invalid, presentationTimeStamp: pts, decodeTimeStamp: .invalid)
        var copy: CMSampleBuffer?
        CMSampleBufferCreateCopyWithNewTiming(
            allocator: nil, sampleBuffer: sb, sampleTimingEntryCount: 1,
            sampleTimingArray: &timing, sampleBufferOut: &copy)
        return copy
    }

    /// Zeroes the samples in place (mono PCM).
    func silence(_ sb: CMSampleBuffer) {
        var abl = AudioBufferList()
        var block: CMBlockBuffer?
        let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sb, bufferListSizeNeededOut: nil, bufferListOut: &abl,
            bufferListSize: MemoryLayout<AudioBufferList>.size,
            blockBufferAllocator: nil, blockBufferMemoryAllocator: nil, flags: 0, blockBufferOut: &block)
        guard status == noErr else { return }
        for buf in UnsafeMutableAudioBufferListPointer(&abl) {
            if let d = buf.mData { memset(d, 0, Int(buf.mDataByteSize)) }
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        fail("capture stopped: \(error.localizedDescription)")
    }

    /// Repeats the last frame at the stop time (a static screen sends no new frames, and the
    /// file must last until the moment recording stopped), then finalizes.
    func finish(done: @escaping () -> Void) {
        queue.async {
            self.finished = true
            self.timer?.cancel()
            guard self.started else { done(); return }
            let now = CMClockGetTime(CMClockGetHostTimeClock())
            self.fill(upTo: now)
            if let last = self.lastVideo, CMTimeCompare(now, self.lastPTS) > 0,
               let copy = self.retimed(last, now), self.video.isReadyForMoreMediaData {
                self.video.append(copy)
            }
            self.video.markAsFinished()
            self.audio?.markAsFinished()
            self.writer.finishWriting { done() }
        }
    }
}

// ---------- setup ----------

func micDeviceID(_ name: String) -> String? {
    if name == "default" { return AVCaptureDevice.default(for: .audio)?.uniqueID }
    let found = AVCaptureDevice.DiscoverySession(
        deviceTypes: [.microphone, .external], mediaType: .audio, position: .unspecified
    ).devices.first { $0.localizedName == name }
    return found?.uniqueID ?? AVCaptureDevice.default(for: .audio)?.uniqueID
}

func start(mic: Bool) async throws -> (SCStream, Recorder) {
    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
    guard !content.displays.isEmpty else { fail("no display") }
    // Same order as CGGetActiveDisplayList (main display first).
    let mainID = CGMainDisplayID()
    let displays = content.displays.sorted { a, b in (a.displayID == mainID ? 0 : 1) < (b.displayID == mainID ? 0 : 1) }
    let display = displays.first { $0.displayID == displayID } ?? displays[min(displayIndex, displays.count - 1)]
    let excluded = content.applications.filter { $0.processID == excludePid }
    let filter = SCContentFilter(display: display, excludingApplications: excluded, exceptingWindows: [])

    // Native pixel size (of the region, when there is one).
    let mode = CGDisplayCopyDisplayMode(display.displayID)
    let scale = Double(mode?.pixelWidth ?? display.width) / Double(max(display.width, 1))
    var width = (mode?.pixelWidth ?? display.width) / 2 * 2
    var height = (mode?.pixelHeight ?? display.height) / 2 * 2
    if let r = rect {
        width = Int((r.width * scale).rounded()) / 2 * 2
        height = Int((r.height * scale).rounded()) / 2 * 2
    }

    let config = SCStreamConfiguration()
    config.width = width
    config.height = height
    if let r = rect { config.sourceRect = r }
    config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
    config.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
    config.showsCursor = false // Clipper draws its own cursor
    config.queueDepth = 8
    if mic, let name = micName {
        config.captureMicrophone = true
        config.microphoneCaptureDeviceID = micDeviceID(name)
    }

    let rec = try Recorder(url: URL(fileURLWithPath: outPath), width: width, height: height, fps: fps, mic: mic && micName != nil)
    rec.muted = startMuted
    rec.fps = fps
    let stream = SCStream(filter: filter, configuration: config, delegate: rec)
    try stream.addStreamOutput(rec, type: .screen, sampleHandlerQueue: rec.queue)
    if mic && micName != nil {
        try stream.addStreamOutput(rec, type: .microphone, sampleHandlerQueue: rec.queue)
    }
    try await stream.startCapture()
    rec.queue.async { rec.startRepeating() }
    return (stream, rec)
}

let session: (SCStream, Recorder)
let ready = DispatchSemaphore(value: 0)
var started: (SCStream, Recorder)?
Task {
    do {
        started = try await start(mic: true)
    } catch {
        if micName != nil {
            // Usually no Microphone permission: record the screen alone.
            say("nomic")
            do { started = try await start(mic: false) } catch { fail(error.localizedDescription) }
        } else {
            fail(error.localizedDescription)
        }
    }
    ready.signal()
}
ready.wait()
session = started!

// ---------- commands ----------

DispatchQueue.global().async {
    while let line = readLine(strippingNewline: true) {
        let cmd = line.trimmingCharacters(in: .whitespaces)
        if cmd.hasPrefix("q") { break }
        if cmd == "m1" { session.1.queue.async { session.1.muted = true } }
        if cmd == "m0" { session.1.queue.async { session.1.muted = false } }
    }
    // "q" or stdin closed: stop.
    Task {
        try? await session.0.stopCapture()
        session.1.finish { exit(session.1.writer.status == .failed ? 1 : 0) }
    }
}

dispatchMain()
