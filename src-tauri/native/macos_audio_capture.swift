import AVFoundation
import Foundation

guard CommandLine.arguments.count == 2 else {
    fputs("usage: redkey-audio-capture <output.wav>\n", stderr)
    exit(2)
}

let outputURL = URL(fileURLWithPath: CommandLine.arguments[1])
guard AVCaptureDevice.default(for: .audio) != nil else {
    fputs("没有可用的麦克风输入\n", stderr)
    exit(3)
}

let settings: [String: Any] = [
    AVFormatIDKey: kAudioFormatLinearPCM,
    AVSampleRateKey: 16_000,
    AVNumberOfChannelsKey: 1,
    AVLinearPCMBitDepthKey: 16,
    AVLinearPCMIsFloatKey: false,
    AVLinearPCMIsBigEndianKey: false,
]

let recorder: AVAudioRecorder
do {
    recorder = try AVAudioRecorder(url: outputURL, settings: settings)
} catch {
    fputs("无法创建音频文件：\(error.localizedDescription)\n", stderr)
    exit(5)
}

guard recorder.prepareToRecord(), recorder.record() else {
    fputs("无法启动麦克风，请检查输入设备和权限\n", stderr)
    exit(4)
}

recorder.isMeteringEnabled = true

print("READY")
fflush(stdout)

// 周期性输出音量等级到 stdout，供 Rust 端读取用于音频可视化。
// averagePower 返回 dB 值（-160..0），映射到 0.0..1.0 与 Windows 端一致。
let levelQueue = DispatchQueue(label: "redkey.audio.level")
let levelTimer = DispatchSource.makeTimerSource(queue: levelQueue)
levelTimer.schedule(deadline: .now() + .milliseconds(100), repeating: .milliseconds(50))
let meterRecorder = recorder
levelTimer.setEventHandler {
    meterRecorder.updateMeters()
    let db = meterRecorder.averagePower(forChannel: 0)
    let level = max(0.0, min(1.0, (db + 60.0) / 60.0))
    fputs("LEVEL:\(String(format: "%.4f", level))\n", stdout)
    fflush(stdout)
}
levelTimer.resume()

_ = readLine()
levelTimer.cancel()
recorder.stop()

do {
    let file = try AVAudioFile(forReading: outputURL)
    guard file.length > 0 else {
        fputs("麦克风未返回任何音频数据\n", stderr)
        exit(6)
    }
} catch {
    fputs("无法校验音频文件：\(error.localizedDescription)\n", stderr)
    exit(6)
}
