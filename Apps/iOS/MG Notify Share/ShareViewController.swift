import Combine
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// "MG Notify" in the share sheet for audio files: adds them to MG Notify's
/// notification sounds, as the Android app's share target does. One sound
/// shared on its own becomes the notification sound; the app tells the server
/// the next time it opens (the extension can't reach the account).
final class ShareViewController: UIViewController {
    override func viewDidLoad() {
        super.viewDidLoad()
        let model = ShareModel(context: extensionContext)
        let host = UIHostingController(rootView: ShareView(model: model))
        addChild(host)
        host.view.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(host.view)
        NSLayoutConstraint.activate([
            host.view.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            host.view.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            host.view.topAnchor.constraint(equalTo: view.topAnchor),
            host.view.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
        host.didMove(toParent: self)
        Task { await model.load() }
    }
}

@MainActor
final class ShareModel: ObservableObject {
    struct SharedSound: Identifiable {
        let id = UUID()
        let name: String
        /// A copy the extension owns; the shared file is only lent while loading.
        let url: URL?
        var problem: String?
    }

    @Published var sounds: [SharedSound] = []
    @Published var isLoading = true
    @Published var isAdding = false
    @Published var result: String?

    private weak var context: NSExtensionContext?
    private let folder = FileManager.default.temporaryDirectory.appending(path: "SharedSounds-" + UUID().uuidString, directoryHint: .isDirectory)

    init(context: NSExtensionContext?) {
        self.context = context
    }

    var addable: [SharedSound] {
        sounds.filter { $0.problem == nil && $0.url != nil }
    }

    func load() async {
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let providers = (context?.inputItems as? [NSExtensionItem] ?? [])
            .flatMap { $0.attachments ?? [] }
            .filter { $0.hasItemConformingToTypeIdentifier(UTType.audio.identifier) }
        var loaded: [SharedSound] = []
        for provider in providers {
            let fallbackName = provider.suggestedName ?? "Sound"
            guard let url = await copy(from: provider) else {
                loaded.append(SharedSound(name: fallbackName, url: nil, problem: SoundLibrary.ImportError.unsupported.errorDescription))
                continue
            }
            var sound = SharedSound(name: url.deletingPathExtension().lastPathComponent, url: url)
            do {
                try await SoundLibrary.check(url)
            } catch {
                sound.problem = error.localizedDescription
            }
            loaded.append(sound)
        }
        sounds = loaded
        isLoading = false
    }

    /// The shared file, copied out under its own name (or the suggested one).
    private func copy(from provider: NSItemProvider) async -> URL? {
        let folder = folder
        let suggested = provider.suggestedName
        return await withCheckedContinuation { continuation in
            _ = provider.loadFileRepresentation(for: .audio, openInPlace: false) { url, _, _ in
                guard let url else {
                    continuation.resume(returning: nil)
                    return
                }
                var name = url.lastPathComponent
                if let suggested, !suggested.isEmpty, URL(fileURLWithPath: suggested).pathExtension.isEmpty {
                    name = suggested + "." + url.pathExtension
                }
                let destination = folder.appending(path: name)
                try? FileManager.default.removeItem(at: destination)
                do {
                    try FileManager.default.copyItem(at: url, to: destination)
                    continuation.resume(returning: destination)
                } catch {
                    continuation.resume(returning: nil)
                }
            }
        }
    }

    func add() async {
        isAdding = true
        var added: [String] = []
        for index in sounds.indices where sounds[index].problem == nil {
            guard let url = sounds[index].url else { continue }
            do {
                added.append(try await SoundLibrary.importSound(from: url))
            } catch {
                sounds[index].problem = error.localizedDescription
            }
        }
        isAdding = false
        guard !added.isEmpty else {
            result = "No sounds were added."
            UIAccessibility.post(notification: .announcement, argument: result)
            return
        }
        let title = { (fileName: String) in URL(fileURLWithPath: fileName).deletingPathExtension().lastPathComponent }
        if added.count == 1 {
            // As in the app: a sound added on its own becomes the notification sound.
            SoundLibrary.sharedDefaults.set(added[0], forKey: "selectedSound")
            SoundLibrary.sharedDefaults.set(true, forKey: "pendingSelectedSoundSync")
            result = "Added \u{201C}\(title(added[0]))\u{201D}. It becomes your notification sound the next time you open MG Notify."
        } else {
            result = "Added \(added.count) sounds. Choose one in MG Notify under Settings, Notification Sound."
        }
        UIAccessibility.post(notification: .announcement, argument: result)
    }

    func finish() {
        try? FileManager.default.removeItem(at: folder)
        context?.completeRequest(returningItems: nil)
    }

    func cancel() {
        try? FileManager.default.removeItem(at: folder)
        context?.cancelRequest(withError: CocoaError(.userCancelled))
    }
}

struct ShareView: View {
    @ObservedObject var model: ShareModel

    var body: some View {
        NavigationStack {
            content
                .navigationTitle("Add Sounds")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    if model.result == nil {
                        ToolbarItem(placement: .cancellationAction) {
                            Button("Cancel") { model.cancel() }
                        }
                        ToolbarItem(placement: .confirmationAction) {
                            Button("Add") { Task { await model.add() } }
                                .disabled(model.isLoading || model.isAdding || model.addable.isEmpty)
                        }
                    } else {
                        ToolbarItem(placement: .confirmationAction) {
                            Button("Done") { model.finish() }
                        }
                    }
                }
        }
    }

    @ViewBuilder
    private var content: some View {
        if model.isLoading {
            ProgressView("Reading sounds…")
        } else if model.sounds.isEmpty {
            ContentUnavailableView("No Sounds", systemImage: "speaker.slash", description: Text("Share audio files to add them as notification sounds."))
        } else {
            List {
                Section {
                    ForEach(model.sounds) { sound in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(sound.name)
                            if let problem = sound.problem {
                                Text(problem)
                                    .font(.footnote)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .accessibilityElement(children: .combine)
                    }
                } header: {
                    Text(header)
                } footer: {
                    Text("Sounds must be 30 seconds or less. MP3, M4A and other formats are converted so iPhone can play them.")
                }
                if let result = model.result {
                    Section {
                        Text(result)
                    }
                }
            }
            .overlay {
                if model.isAdding {
                    ProgressView("Adding…")
                }
            }
        }
    }

    private var header: String {
        let count = model.addable.count
        if model.result != nil {
            return "Sounds"
        }
        return count == 1 ? "This sound will be added to MG Notify:" : "These \(count) sounds will be added to MG Notify:"
    }
}
