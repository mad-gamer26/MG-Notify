import AVFoundation
import Foundation

/// The imported notification sounds, shared by the app and its share
/// extension through the app group. iOS plays notification sounds from the
/// app group's Library/Sounds, and only AIFF, WAV and CAF files of 30 seconds
/// or less, so anything else it can read is converted to CAF on the way in.
enum SoundLibrary {
    static let appGroupIdentifier = "group.com.matthewdovi.MG-Notify"
    static let maximumDuration: Double = 30
    static let sharedDefaults = UserDefaults(suiteName: appGroupIdentifier) ?? .standard

    /// Kept as they are; everything else becomes CAF.
    private static let playableExtensions: Set<String> = ["aiff", "aif", "wav", "caf"]

    nonisolated enum ImportError: LocalizedError {
        case tooLong(seconds: Int)
        case unsupported

        var errorDescription: String? {
            switch self {
            case let .tooLong(seconds):
                return "Longer than 30 seconds (\(seconds) seconds)"
            case .unsupported:
                return "Not an audio file MG Notify can use"
            }
        }
    }

    /// Library/Sounds in the app group, where iOS finds notification sounds.
    static func soundsDirectory() -> URL {
        let fileManager = FileManager.default
        let library = fileManager
            .containerURL(forSecurityApplicationGroupIdentifier: appGroupIdentifier)?
            .appending(path: "Library", directoryHint: .isDirectory)
            ?? fileManager.urls(for: .libraryDirectory, in: .userDomainMask)[0]
        let sounds = library.appending(path: "Sounds", directoryHint: .isDirectory)
        if !fileManager.fileExists(atPath: sounds.path) {
            try? fileManager.createDirectory(at: sounds, withIntermediateDirectories: true)
        }
        return sounds
    }

    /// Throws `ImportError` if `url` isn't audio iOS can read, or is too long.
    static func check(_ url: URL) async throws {
        let duration: Double
        do {
            duration = try await AVURLAsset(url: url).load(.duration).seconds
        } catch {
            throw ImportError.unsupported
        }
        guard duration.isFinite, duration > 0 else {
            throw ImportError.unsupported
        }
        // A sliver over (rounding in the file's header) still counts as 30.
        guard duration <= maximumDuration + 0.05 else {
            throw ImportError.tooLong(seconds: Int(duration.rounded()))
        }
    }

    /// Adds the audio at `url` to the sounds and returns its file name: the
    /// same name for AIFF, WAV and CAF, the name with `.caf` for the rest.
    /// A sound with the same name is replaced.
    static func importSound(from url: URL) async throws -> String {
        try await check(url)
        let isPlayable = playableExtensions.contains(url.pathExtension.lowercased())
        let fileName = isPlayable ? url.lastPathComponent : url.deletingPathExtension().lastPathComponent + ".caf"
        let destination = soundsDirectory().appending(path: fileName)
        let staged = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString + "-" + fileName)
        defer { try? FileManager.default.removeItem(at: staged) }
        if isPlayable {
            try FileManager.default.copyItem(at: url, to: staged)
        } else {
            try await Task.detached(priority: .userInitiated) {
                try convertToCAF(from: url, to: staged)
            }.value
        }
        if FileManager.default.fileExists(atPath: destination.path) {
            try FileManager.default.removeItem(at: destination)
        }
        try FileManager.default.moveItem(at: staged, to: destination)
        return fileName
    }

    /// 16-bit PCM in a CAF file, at the source's sample rate (mono or stereo).
    nonisolated private static func convertToCAF(from source: URL, to destination: URL) throws {
        let input: AVAudioFile
        do {
            input = try AVAudioFile(forReading: source)
        } catch {
            throw ImportError.unsupported
        }
        let format = input.processingFormat
        guard (1...2).contains(format.channelCount), input.length > 0 else {
            throw ImportError.unsupported
        }
        let settings: [String: Any] = [
            AVFormatIDKey: kAudioFormatLinearPCM,
            AVSampleRateKey: format.sampleRate,
            AVNumberOfChannelsKey: format.channelCount,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsFloatKey: false,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsNonInterleaved: false,
        ]
        // The output file is closed when it goes out of scope here.
        try {
            let output = try AVAudioFile(forWriting: destination, settings: settings, commonFormat: format.commonFormat, interleaved: format.isInterleaved)
            guard let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 32_768) else {
                throw ImportError.unsupported
            }
            while input.framePosition < input.length {
                try input.read(into: buffer)
                if buffer.frameLength == 0 {
                    break
                }
                try output.write(from: buffer)
            }
        }()
    }
}
