import Foundation
import AVFoundation
import CoreML
import NaturalLanguage
import FluidAudio

private let ultraSpanishRogueEnglishPrefixes = [
    "In ", "in ", "The ", "the ", "And ", "and ", "You ", "you ",
    "What ", "what ", "Sure ", "sure ", "Yeah ", "yeah ", "Other ", "other "
]

public typealias WispParakeetUltraCallback = @convention(c) (
    UnsafeMutableRawPointer?,
    UnsafePointer<CChar>?,
    Bool,
    Double,
    Double,
    Double
) -> Void

private final class UltraLockedBox<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var storage: T

    init(_ initial: T) {
        self.storage = initial
    }

    func get() -> T {
        lock.withLock { storage }
    }

    func set(_ value: T) {
        lock.withLock { storage = value }
    }
}

private final class UltraDownloadProgressPrinter: @unchecked Sendable {
    private let lock = NSLock()
    private let silent: Bool
    private var lastReportedPercent = -5
    private var didPrint = false

    init(silent: Bool) {
        self.silent = silent
    }

    func update(_ progress: DownloadProgress) {
        guard !silent else { return }
        let pct = max(0, min(100, Int((progress.fractionCompleted * 100.0).rounded())))
        let shouldPrint: Bool = lock.withLock {
            if pct >= lastReportedPercent + 5 || (pct == 100 && lastReportedPercent < 100) {
                lastReportedPercent = pct
                didPrint = true
                return true
            }
            return false
        }
        if shouldPrint {
            fputs("\r\u{1B}[2K  ⬇️  Downloading Parakeet Ultra CoreML (\(pct)%)...", stderr)
            fflush(stderr)
        }
    }

    func finish() {
        let printed = lock.withLock { didPrint }
        if printed && !silent {
            fputs("\r\u{1B}[2K  ✅ Parakeet Ultra CoreML model ready.\n", stderr)
            fflush(stderr)
        }
    }
}

private func parseParakeetUltraLanguage(_ languageCode: String?) -> Language? {
    guard let raw = languageCode?.trimmingCharacters(in: .whitespacesAndNewlines),
          !raw.isEmpty
    else {
        return nil
    }
    let lowered = raw.lowercased()
    if lowered == "auto" {
        return nil
    }
    if let direct = Language(rawValue: lowered) {
        return direct
    }
    if let primary = lowered.split(whereSeparator: { $0 == "-" || $0 == "_" }).first {
        return Language(rawValue: String(primary))
    }
    return nil
}

private func loadParakeetUltraManager(modelDir: String?, silent: Bool) async throws -> AsrManager {
    let models: AsrModels
    if let customDir = modelDir?.trimmingCharacters(in: .whitespacesAndNewlines),
       !customDir.isEmpty,
       FileManager.default.fileExists(atPath: customDir)
    {
        models = try AsrModels.loadLocal(from: URL(fileURLWithPath: customDir), version: .ultra)
    } else {
        let printer = UltraDownloadProgressPrinter(silent: silent)
        models = try await AsrModels.downloadAndLoad(
            version: .ultra,
            progressHandler: { progress in
                printer.update(progress)
            }
        )
        printer.finish()
    }

    let tdtConfig = TdtConfig(blankId: AsrModelVersion.ultra.blankId)
    let asrConfig = ASRConfig(
        tdtConfig: tdtConfig,
        encoderHiddenSize: AsrModelVersion.ultra.encoderHiddenSize
    )
    let manager = AsrManager(config: asrConfig, models: models)
    try await manager.loadModels(models)

    // Warm up CoreML / ANE with 0.5s of silence so first real utterance has zero cold-start latency
    var dummyState = TdtDecoderState.make(decoderLayers: await manager.decoderLayerCount)
    let warmupSilence = [Float](repeating: 0.0, count: 8000)
    _ = try? await manager.transcribe(warmupSilence, decoderState: &dummyState, language: nil)

    return manager
}

/// Serial actor that owns the warm CoreML `AsrManager` (.ultra) and processes final & partial decodes.
actor ParakeetUltraInferenceActor {
    private var asrManager: AsrManager?
    private var language: Language?
    private var isReady: Bool = false

    func initialize(modelDir: String?, languageCode: String?, silent: Bool) async -> Bool {
        do {
            self.language = parseParakeetUltraLanguage(languageCode)
            self.asrManager = try await loadParakeetUltraManager(modelDir: modelDir, silent: silent)
            self.isReady = true
            return true
        } catch {
            fputs("[ParakeetUltraBridge] Failed to initialize AsrManager: \(error.localizedDescription)\n", stderr)
            fflush(stderr)
            self.isReady = false
            return false
        }
    }

    func transcribeSegment(samples: [Float]) async -> (text: String, durationSec: Double, decodeMs: Double)? {
        guard isReady, let manager = asrManager, samples.count >= 4800 else { return nil }
        let start = DispatchTime.now()
        do {
            var state = TdtDecoderState.make(decoderLayers: await manager.decoderLayerCount)
            let result = try await manager.transcribe(samples, decoderState: &state, language: self.language)
            let elapsedMs = Double(DispatchTime.now().uptimeNanoseconds - start.uptimeNanoseconds) / 1_000_000.0
            var trimmed = result.text.trimmingCharacters(in: .whitespacesAndNewlines)

            // Keep Spanish rogue-English prefix sanitizer ONLY when targeting Spanish
            if self.language == .spanish && !trimmed.isEmpty {
                for prefix in ultraSpanishRogueEnglishPrefixes {
                    if trimmed.hasPrefix(prefix) {
                        let candidate = String(trimmed.dropFirst(prefix.count)).trimmingCharacters(in: .whitespaces)
                        if !candidate.isEmpty {
                            trimmed = candidate.prefix(1).uppercased() + candidate.dropFirst()
                            break
                        }
                    }
                }

                if let dominant = NLLanguageRecognizer.dominantLanguage(for: trimmed), dominant == .english {
                    trimmed = ""
                }
            }

            guard !trimmed.isEmpty else { return nil }
            let durSec = Double(samples.count) / 16000.0
            return (trimmed, durSec, elapsedMs)
        } catch {
            return nil
        }
    }

    func cleanup() async {
        await asrManager?.cleanup()
        asrManager = nil
        isReady = false
    }
}

final class WispParakeetUltraSession: @unchecked Sendable {
    private let ctx: UnsafeMutableRawPointer?
    private let callback: WispParakeetUltraCallback
    private let actor = ParakeetUltraInferenceActor()
    private let lock = NSLock()
    private let drainSemaphore = DispatchSemaphore(value: 0)

    private var isStopped = false
    private var inFlightCount: Int = 0

    init?(
        modelDir: String?,
        languageCode: String?,
        silent: Bool,
        ctx: UnsafeMutableRawPointer?,
        callback: @escaping WispParakeetUltraCallback
    ) {
        self.ctx = ctx
        self.callback = callback

        let initSem = DispatchSemaphore(value: 0)
        let okBox = UltraLockedBox<Bool>(false)
        let actorRef = self.actor

        Task.detached(priority: .userInitiated) {
            let ok = await actorRef.initialize(
                modelDir: modelDir,
                languageCode: languageCode,
                silent: silent
            )
            okBox.set(ok)
            initSem.signal()
        }
        initSem.wait()

        guard okBox.get() else {
            return nil
        }
    }

    var isBusy: Bool {
        lock.withLock { inFlightCount > 0 }
    }

    func decode(samples: UnsafePointer<Float>, count: Int, isFinal: Bool, segmentStartSec: Double) {
        guard count >= 4800 else { return }

        let shouldRun: Bool = lock.withLock {
            if isStopped { return false }
            if !isFinal && inFlightCount > 0 {
                return false
            }
            inFlightCount += 1
            return true
        }
        guard shouldRun else { return }

        let audioArray = Array(UnsafeBufferPointer(start: samples, count: min(count, 240_000)))

        Task.detached(priority: .userInitiated) { [weak self] in
            guard let self = self else { return }
            defer {
                let signalDrain: Bool = self.lock.withLock {
                    self.inFlightCount = max(0, self.inFlightCount - 1)
                    return self.isStopped && self.inFlightCount == 0
                }
                if signalDrain {
                    self.drainSemaphore.signal()
                }
            }

            guard let res = await self.actor.transcribeSegment(samples: audioArray) else { return }
            let stopped = self.lock.withLock { self.isStopped }
            guard !stopped, !res.text.isEmpty else { return }

            res.text.withCString { cStr in
                self.callback(
                    self.ctx,
                    cStr,
                    isFinal,
                    segmentStartSec,
                    res.durationSec,
                    res.decodeMs
                )
            }
        }
    }

    func stop() {
        let waitDrain: Bool = lock.withLock {
            if isStopped { return false }
            isStopped = true
            return inFlightCount > 0
        }
        if waitDrain {
            _ = drainSemaphore.wait(timeout: .now() + 2.0)
        }

        let cleanSem = DispatchSemaphore(value: 0)
        let actorRef = self.actor
        Task.detached {
            await actorRef.cleanup()
            cleanSem.signal()
        }
        _ = cleanSem.wait(timeout: .now() + 1.0)
    }
}

private func buildUltraSegments(
    from wordTimings: [WordTiming],
    fullText: String,
    totalDurationMs: Int
) -> [[String: Any]] {
    let terminalChars: Set<Character> = [".", "!", "?", "…"]

    var segments: [[String: Any]] = []
    var currentWords: [WordTiming] = []

    func flushSegment() {
        guard let first = currentWords.first, let last = currentWords.last else { return }
        let text = currentWords.map(\.word).joined(separator: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        currentWords.removeAll(keepingCapacity: true)
        guard !text.isEmpty else { return }

        let startMs = max(0, Int((first.startTime * 1000.0).rounded()))
        let endMs = max(startMs + 100, Int((last.endTime * 1000.0).rounded()))
        segments.append([
            "text": text,
            "start_ms": startMs,
            "end_ms": endMs,
        ])
    }

    for (idx, word) in wordTimings.enumerated() {
        if let prev = currentWords.last, word.startTime - prev.endTime >= 0.85 {
            flushSegment()
        }

        currentWords.append(word)

        let endsWithTerminal = word.word.last.map { terminalChars.contains($0) } ?? false
        let segDuration = (currentWords.last?.endTime ?? 0) - (currentWords.first?.startTime ?? 0)
        let hasNext = idx + 1 < wordTimings.count
        let shouldSplitLong = segDuration >= 7.0 || currentWords.count >= 18

        if endsWithTerminal || (shouldSplitLong && hasNext) {
            flushSegment()
        }
    }
    flushSegment()

    let trimmedFull = fullText.trimmingCharacters(in: .whitespacesAndNewlines)
    if segments.isEmpty && !trimmedFull.isEmpty {
        segments.append([
            "text": trimmedFull,
            "start_ms": 0,
            "end_ms": max(100, totalDurationMs),
        ])
    }

    return segments
}

private func encodeUltraJsonCString(_ object: [String: Any]) -> UnsafeMutablePointer<CChar>? {
    guard let data = try? JSONSerialization.data(withJSONObject: object, options: []),
          let str = String(data: data, encoding: .utf8)
    else {
        return strdup("{\"error\":\"Failed to encode Parakeet Ultra JSON response\"}")
    }
    return strdup(str)
}

// MARK: - C ABI Exports

@_cdecl("wisp_parakeet_ultra_available")
public func wisp_parakeet_ultra_available() -> Bool {
    return true
}

@_cdecl("wisp_parakeet_ultra_start")
public func wisp_parakeet_ultra_start(
    _ modelDir: UnsafePointer<CChar>?,
    _ lang: UnsafePointer<CChar>?,
    _ silent: Bool,
    _ ctx: UnsafeMutableRawPointer?,
    _ callback: @escaping WispParakeetUltraCallback
) -> UnsafeMutableRawPointer? {
    let dirStr = modelDir != nil ? String(cString: modelDir!) : nil
    let langStr = lang != nil ? String(cString: lang!) : nil

    guard let session = WispParakeetUltraSession(
        modelDir: dirStr,
        languageCode: langStr,
        silent: silent,
        ctx: ctx,
        callback: callback
    ) else {
        return nil
    }

    return Unmanaged.passRetained(session).toOpaque()
}

@_cdecl("wisp_parakeet_ultra_is_busy")
public func wisp_parakeet_ultra_is_busy(_ handle: UnsafeMutableRawPointer?) -> Bool {
    guard let handle = handle else { return false }
    let session = Unmanaged<WispParakeetUltraSession>.fromOpaque(handle).takeUnretainedValue()
    return session.isBusy
}

@_cdecl("wisp_parakeet_ultra_decode")
public func wisp_parakeet_ultra_decode(
    _ handle: UnsafeMutableRawPointer?,
    _ samples: UnsafePointer<Float>?,
    _ count: Int,
    _ isFinal: Bool,
    _ segmentStartSec: Double
) {
    guard let handle = handle, let samples = samples, count > 0 else { return }
    let session = Unmanaged<WispParakeetUltraSession>.fromOpaque(handle).takeUnretainedValue()
    session.decode(samples: samples, count: count, isFinal: isFinal, segmentStartSec: segmentStartSec)
}

@_cdecl("wisp_parakeet_ultra_stop")
public func wisp_parakeet_ultra_stop(_ handle: UnsafeMutableRawPointer?) {
    guard let handle = handle else { return }
    let unmanaged = Unmanaged<WispParakeetUltraSession>.fromOpaque(handle)
    let session = unmanaged.takeRetainedValue()
    session.stop()
}

@_cdecl("wisp_parakeet_ultra_transcribe_file")
public func wisp_parakeet_ultra_transcribe_file(
    _ audioPath: UnsafePointer<CChar>?,
    _ modelDir: UnsafePointer<CChar>?,
    _ lang: UnsafePointer<CChar>?,
    _ silent: Bool
) -> UnsafeMutablePointer<CChar>? {
    guard let audioPath = audioPath else {
        return encodeUltraJsonCString(["error": "Missing audio path"])
    }

    let pathStr = String(cString: audioPath)
    let dirStr = modelDir != nil ? String(cString: modelDir!) : nil
    let langStr = lang != nil ? String(cString: lang!) : nil
    let language = parseParakeetUltraLanguage(langStr)

    let sem = DispatchSemaphore(value: 0)
    let resultBox = UltraLockedBox<[String: Any]>(["error": "Unknown Parakeet Ultra transcription failure"])

    Task.detached(priority: .userInitiated) {
        do {
            let manager = try await loadParakeetUltraManager(modelDir: dirStr, silent: silent)
            var state = TdtDecoderState.make(decoderLayers: await manager.decoderLayerCount)
            let result = try await manager.transcribe(
                URL(fileURLWithPath: pathStr),
                decoderState: &state,
                language: language
            )
            await manager.cleanup()

            let durationMs = max(0, Int((result.duration * 1000.0).rounded()))
            let processingMs = max(0, Int((result.processingTime * 1000.0).rounded()))
            let wordTimings = buildWordTimings(from: result.tokenTimings ?? [])

            let wordsJson: [[String: Any]] = wordTimings.map { w in
                let sMs = max(0, Int((w.startTime * 1000.0).rounded()))
                let eMs = max(sMs + 20, Int((w.endTime * 1000.0).rounded()))
                return [
                    "text": w.word,
                    "start_ms": sMs,
                    "end_ms": eMs,
                ]
            }

            let segmentsJson = buildUltraSegments(
                from: wordTimings,
                fullText: result.text,
                totalDurationMs: durationMs
            )

            resultBox.set([
                "text": result.text.trimmingCharacters(in: .whitespacesAndNewlines),
                "duration_ms": durationMs,
                "processing_ms": processingMs,
                "segments": segmentsJson,
                "words": wordsJson,
            ])
        } catch {
            resultBox.set(["error": error.localizedDescription])
        }
        sem.signal()
    }

    sem.wait()
    return encodeUltraJsonCString(resultBox.get())
}

@_cdecl("wisp_parakeet_ultra_free_string")
public func wisp_parakeet_ultra_free_string(_ ptr: UnsafeMutablePointer<CChar>?) {
    if let ptr = ptr {
        free(ptr)
    }
}
