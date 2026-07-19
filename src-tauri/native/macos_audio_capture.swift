import AVFoundation
import Foundation

guard CommandLine.arguments.count == 2 else {
    fputs("usage: redkey-audio-capture <output.wav>\n", stderr)
    exit(2)
}

let outputURL = URL(fileURLWithPath: CommandLine.arguments[1])
let engine = AVAudioEngine()
let input = engine.inputNode
let sourceFormat = input.outputFormat(forBus: 0)
guard sourceFormat.sampleRate > 0, sourceFormat.channelCount > 0 else {
    fputs("没有可用的麦克风输入\n", stderr)
    exit(3)
}

let targetFormat = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 16_000, channels: 1, interleaved: true)!
let converter = AVAudioConverter(from: sourceFormat, to: targetFormat)!
let file = try AVAudioFile(forWriting: outputURL, settings: targetFormat.settings, commonFormat: .pcmFormatInt16, interleaved: true)
let queue = DispatchQueue(label: "com.hilith.redkey.audio")

input.installTap(onBus: 0, bufferSize: 4096, format: sourceFormat) { buffer, _ in
    queue.async {
        let ratio = targetFormat.sampleRate / sourceFormat.sampleRate
        let capacity = AVAudioFrameCount(Double(buffer.frameLength) * ratio) + 32
        guard let converted = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: capacity) else { return }
        var consumed = false
        var error: NSError?
        let status = converter.convert(to: converted, error: &error) { _, outStatus in
            if consumed {
                outStatus.pointee = .noDataNow
                return nil
            }
            consumed = true
            outStatus.pointee = .haveData
            return buffer
        }
        if status == .haveData && converted.frameLength > 0 {
            try? file.write(from: converted)
        }
    }
}

do {
    engine.prepare()
    try engine.start()
    print("READY")
    fflush(stdout)
    _ = readLine()
    input.removeTap(onBus: 0)
    engine.stop()
    queue.sync {}
    print("STOPPED")
} catch {
    fputs("无法启动麦克风：\(error.localizedDescription)\n", stderr)
    exit(4)
}
