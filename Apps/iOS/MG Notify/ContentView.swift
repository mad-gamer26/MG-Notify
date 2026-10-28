import AudioToolbox
import AVFoundation
import Combine
import Network
import os
import SwiftUI
import UIKit
import UniformTypeIdentifiers
import UserNotifications
import WebKit

@MainActor
final class MGNotifyModel: NSObject, ObservableObject {
    static let shared = MGNotifyModel()
    private static let logger = Logger(subsystem: "com.matthewdovi.MG-Notify", category: "notifications")
    private static let appGroupIdentifier = "group.com.matthewdovi.MG-Notify"
    private static let sharedDefaults = UserDefaults(suiteName: appGroupIdentifier) ?? .standard

    let serverURL = "https://notify.mad-gamer.com/api"
    let portalURL = "https://notify.mad-gamer.com"

    @Published var email = UserDefaults.standard.string(forKey: "email") ?? ""
    @Published var password = ""
    @Published var authScreenMode: AuthScreenMode = .signIn
    @Published var permissionStatus = "Not requested"
    @Published var isNotificationPermissionDenied = false
    @Published var deviceToken = ""
    @Published var notifications: [AccountNotification] = []
    @Published var importedSounds: [String] = []
    @Published var selectedSound = MGNotifyModel.sharedDefaults.string(forKey: "selectedSound")
        ?? UserDefaults.standard.string(forKey: "selectedSound")
        ?? "default"
    @Published var isAuthenticated = false
    @Published var isBusy = false
    @Published var isVerifyingStoredSession = false
    @Published var accountVerificationFailed = false
    @Published var remoteSignOutNotice = false
    @Published var emailVerificationRequired = false
    @Published var hasInternetConnection = true
    @Published var navigationResetToken = UUID()
    @Published var devicesRefreshToken = UUID()
    @Published var apiKeysRefreshToken = UUID()
    @Published var notificationsRefreshToken = UUID()
    /// An email notification the person tapped, opened once it's in the list.
    @Published var emailNotificationToOpen: String?
    @Published var authErrorMessage = ""
    @Published var verificationMessage = ""
    /// The active account's optional display name; empty when it has none.
    @Published private(set) var displayName = UserDefaults.standard.string(forKey: "displayName") ?? ""
    @Published private(set) var savedAccounts: [SavedAccount] = MGNotifyModel.loadSavedAccounts()
    @Published private(set) var isAddingAccount = false
    /// Notifications that were unread when this visit to the app saw them: shown as "New" until the app is left.
    @Published private(set) var newNotificationIDs: Set<String> = []
    private var markSeenTask: Task<Void, Never>?
    /// Emails opened this visit: seen, even if a reload started before that says otherwise.
    private var openedEmailIDs: Set<String> = []

    private(set) var sessionToken = UserDefaults.standard.string(forKey: "sessionToken") ?? ""
    private(set) var accountID = UserDefaults.standard.string(forKey: "accountID") ?? ""
    private var sessionTokenBeforeAddingAccount: String?
    let deviceID: String
    private var audioPlayer: AVAudioPlayer?
    private var didAttemptLaunchBootstrap = false
    private let pathMonitor = NWPathMonitor()
    private let pathMonitorQueue = DispatchQueue(label: "MGNotify.NetworkMonitor")
    private var isDeletingAccountLocally = false
    /// One live connection per signed-in account while the app is active.
    private var eventStreamTasks: [String: Task<Void, Never>] = [:]
    private var liveUpdatesWanted = false
    /// Notifications already announced, so a push and a stream event aren't both spoken.
    private var announcedNotificationIDs: [String] = []

    override init() {
        let defaults = UserDefaults.standard
        if let existingID = defaults.string(forKey: "deviceID"), !existingID.isEmpty {
            deviceID = existingID
        } else {
            let newID = UUID().uuidString.lowercased()
            defaults.set(newID, forKey: "deviceID")
            deviceID = newID
        }
        super.init()
        migrateSoundStorageIfNeeded()
        selectedSound = Self.sharedDefaults.string(forKey: "selectedSound")
            ?? defaults.string(forKey: "selectedSound")
            ?? selectedSound
        isAuthenticated = !sessionToken.isEmpty && !email.isEmpty
        if savedAccounts.isEmpty, isAuthenticated {
            upsertActiveSavedAccount()
        } else if !isAuthenticated, let account = savedAccounts.first {
            sessionToken = account.sessionToken
            accountID = account.accountID
            email = account.email
            isAuthenticated = true
            saveSettings()
        }
        refreshPermissionStatus()
        reloadImportedSounds()
        startNetworkMonitoring()
    }

    func performLaunchChecksIfNeeded() async {
        guard !didAttemptLaunchBootstrap else {
            return
        }
        didAttemptLaunchBootstrap = true
        isVerifyingStoredSession = true
        defer {
            isVerifyingStoredSession = false
        }
        guard await checkServerHealth() else {
            resetNavigationHierarchy()
            accountVerificationFailed = true
            return
        }
        startLiveUpdates()
        guard !sessionToken.isEmpty, !email.isEmpty else {
            accountVerificationFailed = false
            return
        }
        await verifyStoredSession(showLoadingState: false)
    }

    func retryServerConnection() async {
        isVerifyingStoredSession = true
        defer {
            isVerifyingStoredSession = false
        }
        guard await checkServerHealth() else {
            resetNavigationHierarchy()
            accountVerificationFailed = true
            return
        }
        accountVerificationFailed = false
        startLiveUpdates()
        if !sessionToken.isEmpty, !email.isEmpty {
            await verifyStoredSession(showLoadingState: false)
        }
    }

    func refreshServerAndSession() async {
        guard await checkServerHealth() else {
            resetNavigationHierarchy()
            accountVerificationFailed = true
            stopLiveUpdates()
            return
        }
        accountVerificationFailed = false
        startLiveUpdates()
        if !sessionToken.isEmpty, !email.isEmpty {
            await verifyStoredSession(showLoadingState: false)
        }
    }

    func verifyStoredSession(showLoadingState: Bool = true) async {
        guard !sessionToken.isEmpty, !email.isEmpty else {
            return
        }
        if showLoadingState {
            isVerifyingStoredSession = true
        }
        defer {
            if showLoadingState {
                isVerifyingStoredSession = false
            }
        }
        do {
            let response: AuthMeResponse = try await sendRequestWithoutBody(
                path: "/auth/me",
                method: "GET",
                authenticated: true
            )
            applyAccount(response.account)
            isAuthenticated = true
            accountVerificationFailed = false
            remoteSignOutNotice = false
            emailVerificationRequired = !response.account.email_verified
            authErrorMessage = ""
            verificationMessage = ""
            saveSettings()
            if emailVerificationRequired {
                notifications = []
                resetNavigationHierarchy()
            }
            await completeSignedInSetup()
        } catch {
            if !remoteSignOutNotice {
                resetNavigationHierarchy()
                accountVerificationFailed = true
            }
        }
    }

    /// The account fields the server sends (sign-in, /auth/me, events).
    private func applyAccount(_ account: AccountResponse) {
        email = account.email
        accountID = account.account_id
        displayName = account.display_name ?? ""
    }

    /// What to call the active account: its display name, or else its email.
    var accountName: String { displayName.isEmpty ? email : displayName }

    /// Sets (or, when blank, removes) the active account's display name.
    func setDisplayName(_ name: String) async throws {
        let response: AuthMeResponse = try await sendRequest(
            path: "/account/display-name",
            method: "POST",
            body: DisplayNameRequest(display_name: name.trimmingCharacters(in: .whitespacesAndNewlines)),
            authenticated: true
        )
        applyAccount(response.account)
        saveSettings()
    }

    func refreshPermissionStatus() {
        UNUserNotificationCenter.current().getNotificationSettings { [weak self] settings in
            DispatchQueue.main.async {
                self?.permissionStatus = settings.authorizationStatus.displayText
                self?.isNotificationPermissionDenied = settings.authorizationStatus == .denied
            }
        }
    }

    func openNotificationSettings() {
        guard let url = URL(string: UIApplication.openNotificationSettingsURLString) else {
            return
        }
        UIApplication.shared.open(url)
    }

    func login() async {
        guard !email.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return
        }
        guard !password.isEmpty else {
            return
        }
        isBusy = true
        defer { isBusy = false }
        do {
            let response: LoginResponse = try await self.sendRequest(
                path: "/auth/login",
                method: "POST",
                body: LoginRequest(email: self.email, password: self.password)
            )
            self.sessionToken = response.session.session_token
            self.applyAccount(response.account)
            self.isAuthenticated = true
            self.isAddingAccount = false
            self.sessionTokenBeforeAddingAccount = nil
            self.authScreenMode = .signIn
            self.accountVerificationFailed = false
            self.remoteSignOutNotice = false
            self.emailVerificationRequired = !response.account.email_verified
            self.authErrorMessage = ""
            self.verificationMessage = ""
            self.password = ""
            self.saveSettings()
            if self.emailVerificationRequired {
                self.notifications = []
                self.resetNavigationHierarchy()
            }
            await self.completeSignedInSetup()
        } catch {
            authErrorMessage = error.localizedDescription
        }
    }

    func registerAccount() async {
        guard !email.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return
        }
        guard !password.isEmpty else {
            return
        }
        isBusy = true
        defer { isBusy = false }
        do {
            let response: LoginResponse = try await self.sendRequest(
                path: "/auth/register",
                method: "POST",
                body: LoginRequest(email: self.email, password: self.password)
            )
            self.sessionToken = response.session.session_token
            self.applyAccount(response.account)
            self.isAuthenticated = true
            self.isAddingAccount = false
            self.sessionTokenBeforeAddingAccount = nil
            self.authScreenMode = .signIn
            self.accountVerificationFailed = false
            self.remoteSignOutNotice = false
            self.emailVerificationRequired = !response.account.email_verified
            self.authErrorMessage = ""
            self.verificationMessage = ""
            self.password = ""
            self.saveSettings()
            if self.emailVerificationRequired {
                self.notifications = []
                self.resetNavigationHierarchy()
            }
            await self.completeSignedInSetup()
        } catch {
            authErrorMessage = error.localizedDescription
        }
    }

    func resendVerificationEmail() async {
        guard isAuthenticated, !sessionToken.isEmpty else {
            return
        }
        isBusy = true
        defer { isBusy = false }
        do {
            let _: AcceptedResponse = try await sendRequestWithoutBody(
                path: "/auth/resend-verification",
                method: "POST",
                authenticated: true
            )
            authErrorMessage = ""
            verificationMessage = "A new verification email has been sent."
        } catch {
            verificationMessage = ""
            authErrorMessage = error.localizedDescription
        }
    }

    func requestPasswordReset() async {
        guard !email.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return
        }
        await runRequest {
            let _: EmptyResponse = try await self.sendRequest(
                path: "/auth/request-password-reset",
                method: "POST",
                body: PasswordResetRequest(email: self.email)
            )
        }
    }

    func logout() async {
        guard !sessionToken.isEmpty else {
            forgetLocalAccountData()
            return
        }
        await runRequest {
            _ = try? await self.sendRequestWithoutBody(
                path: "/devices/\(self.deviceID)",
                method: "DELETE",
                authenticated: true
            ) as EmptyResponse

            _ = try? await self.sendRequestWithoutBody(
                path: "/auth/logout",
                method: "POST",
                authenticated: true
            ) as EmptyResponse

            self.forgetLocalAccountData()
        }
    }

    func forgetLocalAccountData() {
        let forgottenSessionToken = sessionToken
        isDeletingAccountLocally = false
        isAddingAccount = false
        sessionTokenBeforeAddingAccount = nil
        sessionToken = ""
        accountID = ""
        email = ""
        displayName = ""
        password = ""
        notifications = []
        deviceToken = ""
        isAuthenticated = false
        accountVerificationFailed = false
        remoteSignOutNotice = false
        emailVerificationRequired = false
        authErrorMessage = ""
        verificationMessage = ""
        stopLiveUpdates()
        resetNavigationHierarchy()
        let defaults = UserDefaults.standard
        defaults.removeObject(forKey: "sessionToken")
        defaults.removeObject(forKey: "email")
        defaults.removeObject(forKey: "accountID")
        defaults.removeObject(forKey: "displayName")
        removeSavedAccount(withSessionToken: forgottenSessionToken)
        if let nextAccount = savedAccounts.first {
            Task {
                await switchToAccount(nextAccount)
            }
        }
    }

    func switchToAccount(_ account: SavedAccount) async {
        isAddingAccount = false
        sessionTokenBeforeAddingAccount = nil
        sessionToken = account.sessionToken
        accountID = account.accountID
        email = account.email
        displayName = account.displayName
        password = ""
        notifications = []
        newNotificationIDs = []
        openedEmailIDs = []
        isAuthenticated = true
        accountVerificationFailed = false
        remoteSignOutNotice = false
        emailVerificationRequired = false
        authErrorMessage = ""
        verificationMessage = ""
        saveSettings()
        resetNavigationHierarchy()
        startLiveUpdates()
        await verifyStoredSession()
    }

    func startAddingAccount() {
        guard !isAddingAccount else {
            return
        }
        sessionTokenBeforeAddingAccount = sessionToken
        isAddingAccount = true
        sessionToken = ""
        accountID = ""
        email = ""
        displayName = ""
        password = ""
        notifications = []
        isAuthenticated = false
        emailVerificationRequired = false
        authScreenMode = .signIn
        authErrorMessage = ""
        verificationMessage = ""
        resetNavigationHierarchy()
    }

    func cancelAddingAccount() async {
        let previousSessionToken = sessionTokenBeforeAddingAccount
        guard let account = savedAccounts.first(where: { $0.sessionToken == previousSessionToken }) ?? savedAccounts.first else {
            isAddingAccount = false
            sessionTokenBeforeAddingAccount = nil
            return
        }
        await switchToAccount(account)
    }

    func isActiveAccount(_ account: SavedAccount) -> Bool {
        !sessionToken.isEmpty && account.sessionToken == sessionToken
    }

    func signOutSavedAccount(_ account: SavedAccount) async {
        if isActiveAccount(account) {
            await logout()
            return
        }
        await runRequest {
            _ = try? await self.sendRequestWithoutBody(
                path: "/devices/\(self.deviceID)",
                method: "DELETE",
                authenticated: true,
                remoteSignOutOn401: false,
                sessionToken: account.sessionToken
            ) as EmptyResponse

            _ = try? await self.sendRequestWithoutBody(
                path: "/auth/logout",
                method: "POST",
                authenticated: true,
                remoteSignOutOn401: false,
                sessionToken: account.sessionToken
            ) as EmptyResponse

            self.removeSavedAccount(withSessionToken: account.sessionToken)
        }
    }

    func handleRemoteSignOut() {
        if isDeletingAccountLocally {
            forgetLocalAccountData()
            return
        }
        guard isAuthenticated || !sessionToken.isEmpty || !email.isEmpty else {
            return
        }
        isAuthenticated = false
        isBusy = false
        isVerifyingStoredSession = false
        accountVerificationFailed = false
        remoteSignOutNotice = true
        emailVerificationRequired = false
        authErrorMessage = ""
        verificationMessage = ""
        notifications = []
        deviceToken = ""
        stopLiveUpdates()
        resetNavigationHierarchy()
    }

    func updateDeviceToken(_ token: String) {
        deviceToken = token
        Task {
            if isAuthenticated {
                await registerCurrentDevice()
                if !emailVerificationRequired {
                    await fetchNotifications()
                }
            }
            await registerOtherSavedAccountDevices()
        }
    }

    func updateRegistrationError(_: Error) {
    }

    func receiveNotification(userInfo: [AnyHashable: Any], openedByUser: Bool = false) async -> PushHandlingResult {
        if openedByUser,
           let payload = userInfo["mg_notify"] as? [String: Any],
           payload["email"] as? Bool == true,
           let notificationID = payload["notification_id"] as? String, !notificationID.isEmpty {
            emailNotificationToOpen = notificationID
        }
        if let pushAccountID = pushAccountID(from: userInfo), !accountID.isEmpty, pushAccountID != accountID {
            return await receiveNotificationForOtherAccount(
                accountID: pushAccountID,
                userInfo: userInfo,
                openedByUser: openedByUser
            )
        }
        // With the app open, the live connection has usually brought the same
        // change already; only refetch what it hasn't.
        let isStreaming = liveUpdatesWanted && eventStreamTasks[sessionToken] != nil
        if let event = pushEvent(from: userInfo) {
            let isRefreshOnly = [.devicesUpdated, .apiKeysUpdated, .notificationsUpdated].contains(event)
            if !(isStreaming && isRefreshOnly) {
                handlePushEvent(event)
            }
            return .remoteEvent
        }
        guard isAuthenticated else {
            return .ignored
        }
        let pushedID = (userInfo["mg_notify"] as? [String: Any])?["notification_id"] as? String
        if pushedID == nil || !notifications.contains(where: { $0.notification_id == pushedID }) {
            await fetchNotifications()
        }
        return .refreshed
    }

    func registerCurrentDevice() async {
        guard isAuthenticated else {
            return
        }
        guard !deviceToken.isEmpty else {
            return
        }
        await runRequest {
            let _: DeviceRegistrationResponse = try await self.sendRequest(
                path: "/devices/register",
                method: "POST",
                body: DeviceRegistrationRequest(
                    device_id: self.deviceID,
                    device_name: UIDevice.current.name,
                    apns_token: self.deviceToken,
                    apns_environment: Self.currentAPNsEnvironment(),
                    platform: "ios",
                    notification_sound: self.selectedSound
                ),
                authenticated: true
            )
        }
    }

    func registerOtherSavedAccountDevices() async {
        guard !deviceToken.isEmpty else {
            return
        }
        for account in savedAccounts where !isActiveAccount(account) {
            do {
                let _: DeviceRegistrationResponse = try await sendRequest(
                    path: "/devices/register",
                    method: "POST",
                    body: DeviceRegistrationRequest(
                        device_id: deviceID,
                        device_name: UIDevice.current.name,
                        apns_token: deviceToken,
                        apns_environment: Self.currentAPNsEnvironment(),
                        platform: "ios",
                        notification_sound: selectedSound
                    ),
                    authenticated: true,
                    remoteSignOutOn401: false,
                    sessionToken: account.sessionToken
                )
            } catch RequestError.server(statusCode: 401, _) {
                removeSavedAccount(withSessionToken: account.sessionToken)
            } catch {
            }
        }
    }

    func saveSelectedSound() async {
        saveSelectedSoundPreference()
        for account in savedAccounts where !isActiveAccount(account) {
            let _: DeviceRegistrationResponse? = try? await sendRequest(
                path: "/devices/sound",
                method: "POST",
                body: DeviceSoundRequest(
                    device_id: deviceID,
                    notification_sound: selectedSound
                ),
                authenticated: true,
                remoteSignOutOn401: false,
                sessionToken: account.sessionToken
            )
        }
        guard isAuthenticated else {
            return
        }
        guard !emailVerificationRequired else {
            return
        }
        await runRequest {
            let _: DeviceRegistrationResponse = try await self.sendRequest(
                path: "/devices/sound",
                method: "POST",
                body: DeviceSoundRequest(
                    device_id: self.deviceID,
                    notification_sound: self.selectedSound
                ),
                authenticated: true
            )
        }
    }

    func previewSoundSelection() {
        audioPlayer?.stop()
        audioPlayer = nil

        switch selectedSound {
        case "__none__":
            return
        case "default":
            return
        default:
            playImportedSound(named: selectedSound)
        }
    }

    /// Plays an imported sound. The ambient category follows the silent switch, like a notification.
    private func playImportedSound(named name: String) {
        audioPlayer?.stop()
        audioPlayer = nil
        let url = ensureSoundsDirectory().appending(path: name)
        guard FileManager.default.fileExists(atPath: url.path) else {
            return
        }
        do {
            let audioSession = AVAudioSession.sharedInstance()
            try audioSession.setCategory(.ambient)
            try audioSession.setActive(true)
            let player = try AVAudioPlayer(contentsOf: url)
            player.prepareToPlay()
            player.play()
            audioPlayer = player
        } catch {
        }
    }

    func fetchNotifications() async {
        guard isAuthenticated else {
            return
        }
        guard !emailVerificationRequired else {
            return
        }
        await runRequest {
            let response: NotificationsResponse = try await self.sendRequestWithoutBody(
                path: "/notifications",
                method: "GET",
                authenticated: true
            )
            self.notifications = response.notifications.map { notification in
                var notification = notification
                if self.openedEmailIDs.contains(notification.notification_id) {
                    notification.seen = true
                }
                return notification
            }
            self.newNotificationIDs.formUnion(self.notifications.filter { !$0.seen }.map(\.notification_id))
        }
    }

    func deleteNotification(notificationID: String) async {
        guard isAuthenticated else {
            return
        }
        guard !emailVerificationRequired else {
            return
        }
        await runRequest {
            let _: EmptyResponse = try await self.sendRequestWithoutBody(
                path: "/notifications/\(notificationID)",
                method: "DELETE",
                authenticated: true
            )
            self.notifications.removeAll { $0.notification_id == notificationID }
        }
    }

    func clearNotifications() async {
        guard isAuthenticated else {
            return
        }
        guard !emailVerificationRequired else {
            return
        }
        await runRequest {
            let _: ClearNotificationsResponse = try await self.sendRequestWithoutBody(
                path: "/notifications",
                method: "DELETE",
                authenticated: true
            )
            self.notifications = []
        }
    }

    func loadAccountDevices() async throws -> [AccountDevice] {
        let response: AccountDevicesResponse = try await sendRequestWithoutBody(
            path: "/devices",
            method: "GET",
            authenticated: true
        )
        return response.devices
    }

    func signOutManagedDevice(deviceID: String) async throws {
        let _: SignOutDevicesResponse = try await sendRequestWithoutBody(
            path: "/devices/\(deviceID)/sign-out",
            method: "POST",
            authenticated: true
        )
        if deviceID == self.deviceID {
            handleRemoteSignOut()
        }
    }

    func signOutOtherManagedDevices() async throws {
        let _: SignOutDevicesResponse = try await sendRequestWithoutBody(
            path: "/devices/sign-out-other",
            method: "POST",
            authenticated: true
        )
    }

    func loadAPIKeys() async throws -> [ManagedAPIKey] {
        let response: APIKeysResponse = try await sendRequestWithoutBody(
            path: "/api-keys",
            method: "GET",
            authenticated: true
        )
        return response.api_keys
    }

    func createManagedAPIKey(label: String) async throws -> ManagedAPIKey {
        let response: CreateAPIKeyResponse = try await sendRequest(
            path: "/api-keys/create",
            method: "POST",
            body: CreateAPIKeyRequest(label: label)
        , authenticated: true)
        return response.api_key
    }

    func deleteManagedAPIKey(keyID: String) async throws {
        let _: DeleteAPIKeysResponse = try await sendRequest(
            path: "/api-keys/delete",
            method: "POST",
            body: DeleteAPIKeyRequest(key_id: keyID),
            authenticated: true
        )
    }

    func deleteAllManagedAPIKeys() async throws {
        let _: DeleteAPIKeysResponse = try await sendRequestWithoutBody(
            path: "/api-keys/delete-all",
            method: "POST",
            authenticated: true
        )
    }

    func changeAccountEmail(to newEmail: String, password: String) async throws {
        let response: AuthMeResponse = try await sendRequest(
            path: "/account/change-email",
            method: "POST",
            body: ChangeEmailRequest(new_email: newEmail, password: password),
            authenticated: true,
            remoteSignOutOn401: false
        )
        applyAccount(response.account)
        emailVerificationRequired = !response.account.email_verified
        notifications = []
        saveSettings()
        resetNavigationHierarchy()
    }

    func changeAccountPassword(currentPassword: String, newPassword: String) async throws {
        let _: PasswordChangedResponse = try await sendRequest(
            path: "/account/change-password",
            method: "POST",
            body: ChangePasswordRequest(current_password: currentPassword, new_password: newPassword),
            authenticated: true,
            remoteSignOutOn401: false
        )
        handleRemoteSignOut()
    }

    func deleteAccount(password: String) async throws {
        isDeletingAccountLocally = true
        do {
            let _: EmptyResponse = try await sendRequest(
                path: "/account/delete",
                method: "POST",
                body: DeleteAccountRequest(password: password),
                authenticated: true,
                remoteSignOutOn401: false
            )
            forgetLocalAccountData()
        } catch {
            if !isDeletingAccountLocally, sessionToken.isEmpty, email.isEmpty {
                return
            }
            isDeletingAccountLocally = false
            throw error
        }
    }

    func reloadImportedSounds() {
        if let sharedSelectedSound = Self.sharedDefaults.string(forKey: "selectedSound") {
            selectedSound = sharedSelectedSound
        }
        let directory = ensureSoundsDirectory()
        let urls = (try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)) ?? []
        importedSounds = urls
            .filter { $0.hasDirectoryPath == false }
            .map(\.lastPathComponent)
            .sorted { $0.localizedCaseInsensitiveCompare($1) == .orderedAscending }
        if selectedSound != "__none__" && selectedSound != "default" && !importedSounds.contains(selectedSound) {
            selectedSound = "default"
        }
    }

    /// Adds a sound (converted to CAF if iPhone can't play it as it is);
    /// false if it's longer than 30 seconds or not audio iOS can read.
    func importSound(from url: URL) async throws -> Bool {
        do {
            _ = try await SoundLibrary.importSound(from: url)
        } catch is SoundLibrary.ImportError {
            return false
        }
        reloadImportedSounds()
        return true
    }

    func importSounds(from urls: [URL]) async -> Bool {
        var didImportAtLeastOne = false
        for url in urls {
            do {
                let didImport = try await importSound(from: url)
                didImportAtLeastOne = didImportAtLeastOne || didImport
            } catch {
            }
        }
        return didImportAtLeastOne
    }

    func deleteImportedSound(named name: String) {
        let fileManager = FileManager.default
        let url = ensureSoundsDirectory().appending(path: name)
        if fileManager.fileExists(atPath: url.path) {
            try? fileManager.removeItem(at: url)
        }
        if selectedSound == name {
            selectedSound = "default"
            saveSelectedSoundPreference()
            if isAuthenticated {
                Task {
                    await saveSelectedSound()
                }
            }
        }
        reloadImportedSounds()
    }

    func deleteAllImportedSounds() {
        let fileManager = FileManager.default
        for name in importedSounds {
            let url = ensureSoundsDirectory().appending(path: name)
            if fileManager.fileExists(atPath: url.path) {
                try? fileManager.removeItem(at: url)
            }
        }
        selectedSound = "default"
        saveSelectedSoundPreference()
        reloadImportedSounds()
        if isAuthenticated {
            Task {
                await saveSelectedSound()
            }
        }
    }

    func soundOptions() -> [SoundOption] {
        var options = [
            SoundOption(id: "__none__", title: "No Sound"),
            SoundOption(id: "default", title: "System Default"),
        ]
        options.append(contentsOf: importedSounds.map { sound in
            SoundOption(
                id: sound,
                title: URL(fileURLWithPath: sound).deletingPathExtension().lastPathComponent
            )
        })
        return options
    }

    func selectedSoundTitle() -> String {
        soundOptions().first(where: { $0.id == selectedSound })?.title ?? "System Default"
    }

    func pendingSoundImports() -> [PendingSoundImport] {
        let directory = ensurePendingSoundsDirectory()
        let urls = (try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)) ?? []
        return urls
            .filter { $0.hasDirectoryPath == false }
            .map { PendingSoundImport(url: $0) }
            .sorted { $0.fileName.localizedCaseInsensitiveCompare($1.fileName) == .orderedAscending }
    }

    func addPendingSoundImports(selectSingleImportedSound: Bool) async {
        let pendingSounds = pendingSoundImports()
        guard !pendingSounds.isEmpty else {
            return
        }

        let destinationDirectory = ensureSoundsDirectory()
        let fileManager = FileManager.default
        var importedFileNames: [String] = []

        for pendingSound in pendingSounds {
            let destinationURL = destinationDirectory.appending(path: pendingSound.fileName)
            do {
                if fileManager.fileExists(atPath: destinationURL.path) {
                    try fileManager.removeItem(at: destinationURL)
                }
                try fileManager.copyItem(at: pendingSound.url, to: destinationURL)
                importedFileNames.append(pendingSound.fileName)
            } catch {
            }
        }

        clearPendingSoundImports()
        reloadImportedSounds()

        if selectSingleImportedSound, importedFileNames.count == 1 {
            selectedSound = importedFileNames[0]
            await saveSelectedSound()
        }
    }

    func clearPendingSoundImports() {
        let directory = ensurePendingSoundsDirectory()
        try? FileManager.default.removeItem(at: directory)
        Self.sharedDefaults.set(false, forKey: "pendingOpenAddSounds")
    }

    func consumePendingOpenAddSounds() -> Bool {
        let shouldOpen = Self.sharedDefaults.bool(forKey: "pendingOpenAddSounds")
        Self.sharedDefaults.set(false, forKey: "pendingOpenAddSounds")
        return shouldOpen
    }

    func consumePendingSelectedSoundSync() -> Bool {
        let shouldSync = Self.sharedDefaults.bool(forKey: "pendingSelectedSoundSync")
        Self.sharedDefaults.set(false, forKey: "pendingSelectedSoundSync")
        return shouldSync
    }

    func consumePendingOpenSounds() -> Bool {
        let shouldOpen = Self.sharedDefaults.bool(forKey: "pendingOpenSounds")
        Self.sharedDefaults.set(false, forKey: "pendingOpenSounds")
        return shouldOpen
    }

    private func completeSignedInSetup() async {
        await ensurePushSetup()
        await registerCurrentDevice()
        if !emailVerificationRequired {
            await fetchNotifications()
        }
        await registerOtherSavedAccountDevices()
    }

    private func checkServerHealth() async -> Bool {
        do {
            let _: HealthResponse = try await sendRequestWithoutBody(
                path: "/health",
                method: "GET"
            )
            return true
        } catch {
            return false
        }
    }

    /// Opens a live connection to the server's event stream (GET /events) for
    /// every signed-in account while the app is active, so changes made
    /// anywhere show up at once. Nothing is polled; a dropped connection leads
    /// to one health check.
    func startLiveUpdates() {
        liveUpdatesWanted = true
        syncEventStreams()
    }

    func stopLiveUpdates() {
        liveUpdatesWanted = false
        for task in eventStreamTasks.values {
            task.cancel()
        }
        eventStreamTasks.removeAll()
    }

    private func syncEventStreams() {
        var tokens = Set(savedAccounts.map(\.sessionToken))
        if isAuthenticated, !sessionToken.isEmpty {
            tokens.insert(sessionToken)
        }
        for (token, task) in eventStreamTasks where !tokens.contains(token) {
            task.cancel()
            eventStreamTasks[token] = nil
        }
        for token in tokens where eventStreamTasks[token] == nil {
            eventStreamTasks[token] = Task { [weak self] in
                await self?.runEventStream(token: token)
            }
        }
    }

    /// One account's stream: reconnects by itself, and hands each event to
    /// handleStreamEvent. A "ready" after a reconnect carries "reconnected".
    private func runEventStream(token: String) async {
        let configuration = URLSessionConfiguration.default
        // The server sends a keep-alive every 20 seconds.
        configuration.timeoutIntervalForRequest = 60
        configuration.timeoutIntervalForResource = 7 * 24 * 3600
        let session = URLSession(configuration: configuration)
        defer { session.invalidateAndCancel() }
        var failures = 0
        var connectedBefore = false
        while !Task.isCancelled {
            guard let baseURL = URL(string: serverURL) else { return }
            var request = URLRequest(url: baseURL.appending(path: "events"))
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
            request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
            do {
                let (bytes, response) = try await session.bytes(for: request)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                if status == 401 {
                    await handleStreamEvent(token: token, name: "unauthorized", data: [:])
                    return
                }
                guard status == 200 else {
                    throw URLError(.badServerResponse)
                }
                failures = 0
                var eventName: String?
                // The server always sends "event:" then "data:" for each event.
                for try await line in bytes.lines {
                    if line.hasPrefix("event:") {
                        eventName = line.dropFirst(6).trimmingCharacters(in: .whitespaces)
                    } else if line.hasPrefix("data:"), let name = eventName {
                        eventName = nil
                        let text = line.dropFirst(5).trimmingCharacters(in: .whitespaces)
                        var data = (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any] ?? [:]
                        if name == "ready" {
                            data["reconnected"] = connectedBefore
                            connectedBefore = true
                        }
                        await handleStreamEvent(token: token, name: name, data: data)
                        if name == "signed_out" || name == "account_deleted" {
                            return
                        }
                    }
                }
            } catch {
                if Task.isCancelled { return }
            }
            if Task.isCancelled { return }
            failures += 1
            await handleStreamEvent(token: token, name: "disconnected", data: ["failures": failures])
            try? await Task.sleep(for: .seconds(min(30, 1 << min(failures - 1, 5))))
        }
    }

    private func decode<T: Decodable>(_ type: T.Type, from object: Any?) -> T? {
        guard let object, JSONSerialization.isValidJSONObject(object),
              let data = try? JSONSerialization.data(withJSONObject: object) else {
            return nil
        }
        return try? JSONDecoder().decode(type, from: data)
    }

    private func handleStreamEvent(token: String, name: String, data: [String: Any]) async {
        let isActive = isAuthenticated && token == sessionToken
        switch name {
        case "ready", "account_updated":
            guard let account = decode(AccountResponse.self, from: data["account"]) else { return }
            guard isActive else {
                updateSavedAccount(token: token, account: account)
                return
            }
            let wasVerificationRequired = emailVerificationRequired
            applyAccount(account)
            emailVerificationRequired = !account.email_verified
            saveSettings()
            if emailVerificationRequired, !wasVerificationRequired {
                notifications = []
                resetNavigationHierarchy()
            } else if !emailVerificationRequired, wasVerificationRequired || (data["reconnected"] as? Bool ?? false) {
                // Newly verified, or back after a dropped connection: catch up.
                await fetchNotifications()
                devicesRefreshToken = UUID()
                apiKeysRefreshToken = UUID()
            }
        case "notification":
            guard let notification = decode(AccountNotification.self, from: data["notification"]) else { return }
            if isActive, !notifications.contains(where: { $0.notification_id == notification.notification_id }) {
                notifications.insert(notification, at: 0)
                if !notification.seen {
                    newNotificationIDs.insert(notification.notification_id)
                }
            }
            let owner = savedAccounts.first { $0.sessionToken == token }?.accountID ?? accountID
            announceNotification(
                accountID: owner,
                notificationID: notification.notification_id,
                title: notification.displayTitle,
                message: notification.message,
                email: notification.isEmailNotification
            )
        case "notification_deleted":
            if isActive, let id = data["notification_id"] as? String {
                notifications.removeAll { $0.notification_id == id }
            }
        case "notifications_cleared":
            if isActive {
                notifications = []
            }
        case "notifications_seen":
            if isActive, let ids = data["notification_ids"] as? [String] {
                for index in notifications.indices where ids.contains(notifications[index].notification_id) {
                    notifications[index].seen = true
                }
            }
        case "devices_updated":
            if isActive { devicesRefreshToken = UUID() }
        case "api_keys_updated":
            if isActive { apiKeysRefreshToken = UUID() }
        case "resync":
            if isActive {
                await fetchNotifications()
                devicesRefreshToken = UUID()
                apiKeysRefreshToken = UUID()
            }
        case "signed_out", "account_deleted", "unauthorized":
            // The stream has ended itself; just forget it.
            eventStreamTasks[token] = nil
            if isActive {
                handleRemoteSignOut()
            } else {
                removeSavedAccount(withSessionToken: token)
            }
        case "disconnected":
            guard isActive, (data["failures"] as? Int) == 1, hasInternetConnection else { return }
            if !(await checkServerHealth()) {
                resetNavigationHierarchy()
                accountVerificationFailed = true
                stopLiveUpdates()
            }
        default:
            break
        }
    }

    /// The notification list is on screen: once it has been for a moment,
    /// unread notifications are marked seen everywhere, and the app badge is
    /// set to what's still unread on this device (all its accounts). They
    /// stay marked "New" here until the app is left. With `syncBadge`, the
    /// server is asked even when nothing here is unread, to correct the badge.
    func scheduleMarkSeen(syncBadge: Bool = false) {
        markSeenTask?.cancel()
        markSeenTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(1.5))
            guard let self, !Task.isCancelled else { return }
            guard UIApplication.shared.applicationState == .active, isAuthenticated, !emailVerificationRequired else { return }
            guard syncBadge || notifications.contains(where: { !$0.seen }) else { return }
            guard let response: MarkSeenResponse = try? await sendRequest(
                path: "/notifications/mark-seen",
                method: "POST",
                body: MarkSeenRequest(),
                authenticated: true
            ) else {
                return
            }
            for index in notifications.indices {
                notifications[index].seen = true
            }
            if let badge = response.badge {
                try? await UNUserNotificationCenter.current().setBadgeCount(badge)
            }
        }
    }

    /// An email was opened: it counts as seen straight away, here and
    /// everywhere else, and loses its "New" marker.
    func markEmailSeen(_ notificationID: String) {
        openedEmailIDs.insert(notificationID)
        newNotificationIDs.remove(notificationID)
        guard let index = notifications.firstIndex(where: { $0.notification_id == notificationID }), !notifications[index].seen else {
            return
        }
        notifications[index].seen = true
        Task {
            guard let response: MarkSeenResponse = try? await sendRequest(
                path: "/notifications/mark-seen",
                method: "POST",
                body: MarkSeenRequest(notification_ids: [notificationID]),
                authenticated: true
            ) else {
                return
            }
            if let badge = response.badge {
                try? await UNUserNotificationCenter.current().setBadgeCount(badge)
            }
        }
    }

    /// The list left the screen (an email opened over it, say): nothing more counts as seen.
    func cancelMarkSeen() {
        markSeenTask?.cancel()
    }

    /// Leaving the app: next time, only notifications that are new by then are marked.
    func forgetNewMarkers() {
        markSeenTask?.cancel()
        newNotificationIDs = []
        openedEmailIDs = []
    }

    /// Another signed-in account's email or display name changed.
    private func updateSavedAccount(token: String, account: AccountResponse) {
        guard let index = savedAccounts.firstIndex(where: { $0.sessionToken == token }) else { return }
        let updated = SavedAccount(
            accountID: account.account_id,
            email: account.email,
            sessionToken: token,
            displayName: account.display_name ?? ""
        )
        guard updated != savedAccounts[index] else { return }
        savedAccounts[index] = updated
        persistSavedAccounts()
    }

    /// A push that arrived while the app is in front: announced instead of shown.
    func announcePush(userInfo: [AnyHashable: Any]) {
        guard
            let aps = userInfo["aps"] as? [String: Any],
            let alert = aps["alert"] as? [String: Any]
        else {
            return
        }
        let payload = userInfo["mg_notify"] as? [String: Any]
        let title = alert["title"] as? String ?? ""
        let body = alert["body"] as? String ?? ""
        let email = payload?["email"] as? Bool ?? false
        let displayTitle = decode(
            AccountNotification.self,
            from: ["notification_id": "", "title": title, "message": body, "created_at": "", "email": email]
        )?.displayTitle ?? title
        announceNotification(
            accountID: payload?["account_id"] as? String ?? accountID,
            notificationID: payload?["notification_id"] as? String,
            title: displayTitle,
            message: body,
            email: email
        )
    }

    /// While the app is open, a new notification is spoken ("New notification
    /// in <account>: <title>, <message>", naming the account only when several
    /// are signed in) and its sound played, instead of showing a banner. Each
    /// one is announced once, however it arrived.
    private func announceNotification(accountID ownerID: String, notificationID: String?, title: String, message: String, email: Bool) {
        guard UIApplication.shared.applicationState == .active else { return }
        if let notificationID, !notificationID.isEmpty {
            guard !announcedNotificationIDs.contains(notificationID) else { return }
            announcedNotificationIDs.append(notificationID)
            if announcedNotificationIDs.count > 200 {
                announcedNotificationIDs.removeFirst()
            }
        }
        let place = savedAccounts.count > 1
            ? " in " + (savedAccounts.first { $0.accountID == ownerID }?.name ?? accountName)
            : ""
        let text = "New notification\(place): \(title), \(email ? "email" : message)"
        Self.logger.debug("Announcing: \(text, privacy: .private)")
        UIAccessibility.post(
            notification: .announcement,
            argument: NSAttributedString(string: text, attributes: [.accessibilitySpeechQueueAnnouncement: true])
        )
        playNotificationSound()
    }

    /// The chosen sound for a notification arriving while the app is open.
    /// "System Default" is iOS's default alert sound: apps can't play the
    /// tone a person picked in Settings, so this is the standard one.
    func playNotificationSound() {
        switch selectedSound {
        case "__none__":
            return
        case "default":
            AudioServicesPlaySystemSound(1007)
        default:
            playImportedSound(named: selectedSound)
        }
    }

    private func ensurePushSetup() async {
        #if DEBUG
        // Automated testing in the simulator, which can't answer the permission prompt.
        if ProcessInfo.processInfo.arguments.contains("-MGNotifySkipNotificationPrompt") {
            refreshPermissionStatus()
            return
        }
        #endif
        let status = await notificationAuthorizationStatus()
        switch status {
        case .notDetermined:
            let granted = await requestAuthorization()
            refreshPermissionStatus()
            if granted {
                UIApplication.shared.registerForRemoteNotifications()
            }
        case .authorized, .provisional, .ephemeral:
            UIApplication.shared.registerForRemoteNotifications()
        case .denied:
            refreshPermissionStatus()
        @unknown default:
            refreshPermissionStatus()
        }
    }

    private static func currentAPNsEnvironment() -> String {
        guard
            let provisioningProfileURL = Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision"),
            let provisioningProfileData = try? Data(contentsOf: provisioningProfileURL),
            let provisioningProfile = String(data: provisioningProfileData, encoding: .isoLatin1),
            let plistStart = provisioningProfile.range(of: "<?xml"),
            let plistEnd = provisioningProfile.range(of: "</plist>")
        else {
            return "production"
        }

        let plistString = String(provisioningProfile[plistStart.lowerBound..<plistEnd.upperBound])
        guard
            let plistData = plistString.data(using: .utf8),
            let plist = try? PropertyListSerialization.propertyList(from: plistData, format: nil) as? [String: Any],
            let entitlements = plist["Entitlements"] as? [String: Any]
        else {
            return "production"
        }

        let environment = (entitlements["aps-environment"] as? String)
            ?? (entitlements["com.apple.developer.aps-environment"] as? String)

        switch environment?.lowercased() {
        case "development", "sandbox":
            return "sandbox"
        case "production":
            return "production"
        default:
            return "production"
        }
    }

    private func notificationAuthorizationStatus() async -> UNAuthorizationStatus {
        await withCheckedContinuation { continuation in
            UNUserNotificationCenter.current().getNotificationSettings { settings in
                continuation.resume(returning: settings.authorizationStatus)
            }
        }
    }

    private func requestAuthorization() async -> Bool {
        await withCheckedContinuation { continuation in
            UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .badge, .sound]) { granted, _ in
                continuation.resume(returning: granted)
            }
        }
    }

    private func ensureSoundsDirectory() -> URL {
        let fileManager = FileManager.default
        let libraryDirectory = fileManager
            .containerURL(forSecurityApplicationGroupIdentifier: Self.appGroupIdentifier)?
            .appending(path: "Library", directoryHint: .isDirectory)
            ?? fileManager.urls(for: .libraryDirectory, in: .userDomainMask)[0]
        let soundsDirectory = libraryDirectory.appending(path: "Sounds", directoryHint: .isDirectory)
        if !fileManager.fileExists(atPath: soundsDirectory.path) {
            try? fileManager.createDirectory(at: soundsDirectory, withIntermediateDirectories: true)
        }
        return soundsDirectory
    }

    private func ensurePendingSoundsDirectory() -> URL {
        let fileManager = FileManager.default
        let libraryDirectory = fileManager
            .containerURL(forSecurityApplicationGroupIdentifier: Self.appGroupIdentifier)?
            .appending(path: "Library", directoryHint: .isDirectory)
            ?? fileManager.urls(for: .libraryDirectory, in: .userDomainMask)[0]
        let pendingSoundsDirectory = libraryDirectory.appending(path: "PendingSoundImports", directoryHint: .isDirectory)
        if !fileManager.fileExists(atPath: pendingSoundsDirectory.path) {
            try? fileManager.createDirectory(at: pendingSoundsDirectory, withIntermediateDirectories: true)
        }
        return pendingSoundsDirectory
    }

    private func migrateSoundStorageIfNeeded() {
        let defaults = UserDefaults.standard
        if Self.sharedDefaults.string(forKey: "selectedSound") == nil,
           let standardSelectedSound = defaults.string(forKey: "selectedSound") {
            Self.sharedDefaults.set(standardSelectedSound, forKey: "selectedSound")
        }

        let fileManager = FileManager.default
        guard let sharedLibraryDirectory = fileManager
            .containerURL(forSecurityApplicationGroupIdentifier: Self.appGroupIdentifier)?
            .appending(path: "Library", directoryHint: .isDirectory)
        else {
            return
        }

        let legacySoundsDirectory = fileManager.urls(for: .libraryDirectory, in: .userDomainMask)[0]
            .appending(path: "Sounds", directoryHint: .isDirectory)
        let sharedSoundsDirectory = sharedLibraryDirectory.appending(path: "Sounds", directoryHint: .isDirectory)
        guard let legacySoundURLs = try? fileManager.contentsOfDirectory(at: legacySoundsDirectory, includingPropertiesForKeys: nil) else {
            return
        }

        // Sounds used to live in the app's own Library/Sounds. They're copied
        // across only when the shared folder doesn't exist yet (the first
        // launch after moving); after that the old folder holds only stale
        // copies, and copying "missing" ones would bring back deleted sounds.
        // So it's removed once its sounds are safe in the shared folder.
        var copyFailed = false
        if !fileManager.fileExists(atPath: sharedSoundsDirectory.path) {
            try? fileManager.createDirectory(at: sharedSoundsDirectory, withIntermediateDirectories: true)
            for legacySoundURL in legacySoundURLs where !legacySoundURL.hasDirectoryPath {
                let destinationURL = sharedSoundsDirectory.appending(path: legacySoundURL.lastPathComponent)
                do {
                    try fileManager.copyItem(at: legacySoundURL, to: destinationURL)
                } catch {
                    copyFailed = true
                }
            }
        }
        if !copyFailed {
            try? fileManager.removeItem(at: legacySoundsDirectory)
        }
    }

    private func saveSelectedSoundPreference() {
        Self.sharedDefaults.set(selectedSound, forKey: "selectedSound")
        UserDefaults.standard.set(selectedSound, forKey: "selectedSound")
    }

    private func saveSettings() {
        let defaults = UserDefaults.standard
        defaults.set(email, forKey: "email")
        defaults.set(sessionToken, forKey: "sessionToken")
        defaults.set(accountID, forKey: "accountID")
        defaults.set(displayName, forKey: "displayName")
        upsertActiveSavedAccount()
        saveSelectedSoundPreference()
    }

    private static func loadSavedAccounts() -> [SavedAccount] {
        guard let data = UserDefaults.standard.data(forKey: "savedAccounts") else {
            return []
        }
        return (try? JSONDecoder().decode([SavedAccount].self, from: data)) ?? []
    }

    private func persistSavedAccounts() {
        if let data = try? JSONEncoder().encode(savedAccounts) {
            UserDefaults.standard.set(data, forKey: "savedAccounts")
        }
        if liveUpdatesWanted {
            syncEventStreams()
        }
    }

    private func upsertActiveSavedAccount() {
        guard !sessionToken.isEmpty, !email.isEmpty else {
            return
        }
        let account = SavedAccount(accountID: accountID, email: email, sessionToken: sessionToken, displayName: displayName)
        if let index = savedAccounts.firstIndex(where: {
            $0.sessionToken == sessionToken || (!accountID.isEmpty && $0.accountID == accountID)
        }) {
            savedAccounts[index] = account
        } else {
            savedAccounts.append(account)
        }
        persistSavedAccounts()
    }

    private func removeSavedAccount(withSessionToken token: String) {
        guard !token.isEmpty else {
            return
        }
        savedAccounts.removeAll { $0.sessionToken == token }
        persistSavedAccounts()
    }

    private func runRequest(action: @escaping () async throws -> Void) async {
        isBusy = true
        defer { isBusy = false }
        do {
            try await action()
        } catch {
        }
    }

    private func sendRequest<RequestBody: Encodable, ResponseBody: Decodable>(
        path: String,
        method: String,
        body: RequestBody,
        authenticated: Bool = false,
        remoteSignOutOn401: Bool = true,
        sessionToken overrideSessionToken: String? = nil
    ) async throws -> ResponseBody {
        var request = try makeRequest(
            path: path,
            method: method,
            authenticated: authenticated,
            sessionToken: overrideSessionToken
        )
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(body)
        let (data, response) = try await URLSession.shared.data(for: request)
        return try decodeResponse(
            data: data,
            response: response,
            remoteSignOutOn401: remoteSignOutOn401,
            appliesToActiveAccount: overrideSessionToken == nil
        )
    }

    private func sendRequestWithoutBody<ResponseBody: Decodable>(
        path: String,
        method: String,
        authenticated: Bool = false,
        remoteSignOutOn401: Bool = true,
        sessionToken overrideSessionToken: String? = nil
    ) async throws -> ResponseBody {
        let request = try makeRequest(
            path: path,
            method: method,
            authenticated: authenticated,
            sessionToken: overrideSessionToken
        )
        let (data, response) = try await URLSession.shared.data(for: request)
        return try decodeResponse(
            data: data,
            response: response,
            remoteSignOutOn401: remoteSignOutOn401,
            appliesToActiveAccount: overrideSessionToken == nil
        )
    }

    private func makeRequest(
        path: String,
        method: String,
        authenticated: Bool,
        sessionToken overrideSessionToken: String? = nil
    ) throws -> URLRequest {
        guard let baseURL = URL(string: serverURL) else {
            throw RequestError.invalidURL
        }
        let url = baseURL.appending(path: path.trimmingCharacters(in: CharacterSet(charactersIn: "/")))
        var request = URLRequest(url: url)
        request.httpMethod = method
        if authenticated {
            let token = overrideSessionToken ?? sessionToken
            guard !token.isEmpty else {
                throw RequestError.missingSession
            }
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        return request
    }

    private func decodeResponse<ResponseBody: Decodable>(
        data: Data,
        response: URLResponse,
        remoteSignOutOn401: Bool,
        appliesToActiveAccount: Bool = true
    ) throws -> ResponseBody {
        guard let httpResponse = response as? HTTPURLResponse else {
            throw RequestError.invalidResponse
        }
        guard (200 ... 299).contains(httpResponse.statusCode) else {
            let serverError = try? JSONDecoder().decode(ServerErrorResponse.self, from: data)
            let detailText = serverError?.detailText ?? "Unknown server error"
            guard appliesToActiveAccount else {
                throw RequestError.server(statusCode: httpResponse.statusCode, detail: detailText)
            }
            if httpResponse.statusCode == 401, remoteSignOutOn401, !sessionToken.isEmpty {
                handleRemoteSignOut()
            } else if httpResponse.statusCode == 403, detailText == "Email verification is required" {
                emailVerificationRequired = true
                accountVerificationFailed = false
                remoteSignOutNotice = false
                notifications = []
                verificationMessage = ""
                resetNavigationHierarchy()
            }
            throw RequestError.server(statusCode: httpResponse.statusCode, detail: detailText)
        }
        return try JSONDecoder().decode(ResponseBody.self, from: data)
    }

    private func pushEvent(from userInfo: [AnyHashable: Any]) -> MGNotifyPushEvent? {
        guard
            let payload = userInfo["mg_notify"] as? [String: Any],
            let rawEvent = payload["event"] as? String
        else {
            return nil
        }
        return MGNotifyPushEvent(rawValue: rawEvent)
    }

    private func pushAccountID(from userInfo: [AnyHashable: Any]) -> String? {
        guard
            let payload = userInfo["mg_notify"] as? [String: Any],
            let accountID = payload["account_id"] as? String,
            !accountID.isEmpty
        else {
            return nil
        }
        return accountID
    }

    private func receiveNotificationForOtherAccount(
        accountID otherAccountID: String,
        userInfo: [AnyHashable: Any],
        openedByUser: Bool
    ) async -> PushHandlingResult {
        let otherAccount = savedAccounts.first { $0.accountID == otherAccountID }
        if let event = pushEvent(from: userInfo) {
            if let otherAccount, event == .signedOut || event == .accountDeleted {
                removeSavedAccount(withSessionToken: otherAccount.sessionToken)
            }
            return .remoteEvent
        }
        guard openedByUser, let otherAccount else {
            return .ignored
        }
        await switchToAccount(otherAccount)
        return .refreshed
    }

    private func handlePushEvent(_ event: MGNotifyPushEvent) {
        switch event {
        case .signedOut, .accountDeleted:
            handleRemoteSignOut()
        case .devicesUpdated:
            devicesRefreshToken = UUID()
        case .apiKeysUpdated:
            apiKeysRefreshToken = UUID()
        case .notificationsUpdated:
            notificationsRefreshToken = UUID()
        case .emailVerified, .emailVerificationRequired:
            Task {
                await verifyStoredSession(showLoadingState: false)
            }
        }
    }

    private func resetNavigationHierarchy() {
        navigationResetToken = UUID()
    }

    private func startNetworkMonitoring() {
        pathMonitor.pathUpdateHandler = { [weak self] path in
            let hasInternetConnection = path.status == .satisfied
            Task { @MainActor [weak self] in
                self?.hasInternetConnection = hasInternetConnection
            }
        }
        pathMonitor.start(queue: pathMonitorQueue)
    }
}

struct SavedAccount: Codable, Identifiable, Hashable {
    let accountID: String
    let email: String
    let sessionToken: String
    /// Optional; empty when the account has none.
    var displayName: String = ""

    var id: String { sessionToken }

    /// What to call the account: its display name, or else its email.
    var name: String { displayName.isEmpty ? email : displayName }

    init(accountID: String, email: String, sessionToken: String, displayName: String = "") {
        self.accountID = accountID
        self.email = email
        self.sessionToken = sessionToken
        self.displayName = displayName
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        accountID = try container.decode(String.self, forKey: .accountID)
        email = try container.decode(String.self, forKey: .email)
        sessionToken = try container.decode(String.self, forKey: .sessionToken)
        // Saved before display names existed.
        displayName = try container.decodeIfPresent(String.self, forKey: .displayName) ?? ""
    }
}

enum PushHandlingResult {
    case ignored
    case refreshed
    case remoteEvent
}

enum MGNotifyPushEvent: String {
    case signedOut = "signed_out"
    case accountDeleted = "account_deleted"
    case devicesUpdated = "devices_updated"
    case apiKeysUpdated = "api_keys_updated"
    case notificationsUpdated = "notifications_updated"
    case emailVerified = "email_verified"
    case emailVerificationRequired = "email_verification_required"
}

enum RequestError: LocalizedError {
    case invalidURL
    case invalidResponse
    case missingSession
    case server(statusCode: Int, detail: String)

    var errorDescription: String? {
        switch self {
        case .invalidURL:
            return "The server URL is invalid."
        case .invalidResponse:
            return "The server did not return a valid HTTP response."
        case .missingSession:
            return "The current account session is missing."
        case let .server(_, detail):
            return detail
        }
    }
}

extension UNAuthorizationStatus {
    var displayText: String {
        switch self {
        case .notDetermined:
            return "Not requested"
        case .denied:
            return "Denied"
        case .authorized:
            return "Enabled"
        case .provisional:
            return "Enabled"
        case .ephemeral:
            return "Enabled"
        @unknown default:
            return "Unknown"
        }
    }
}

struct LoginRequest: Encodable {
    let email: String
    let password: String
}

struct PasswordResetRequest: Encodable {
    let email: String
}

struct DeviceRegistrationRequest: Encodable {
    let device_id: String
    let device_name: String
    let apns_token: String
    let apns_environment: String
    let platform: String
    let notification_sound: String
    /// This app keeps its badge up to date, so the server may send badge numbers.
    var badges = true
}

struct MarkSeenRequest: Encodable {
    /// Only these; every unread notification when nil (left out of the JSON).
    var notification_ids: [String]? = nil
}

struct MarkSeenResponse: Decodable {
    let marked: Int
    let unread_count: Int
    let badge: Int?
}

struct DeviceSoundRequest: Encodable {
    let device_id: String
    let notification_sound: String
}

struct CreateAPIKeyRequest: Encodable {
    let label: String
}

struct DeleteAPIKeyRequest: Encodable {
    let key_id: String
}

struct ChangeEmailRequest: Encodable {
    let new_email: String
    let password: String
}

struct ChangePasswordRequest: Encodable {
    let current_password: String
    let new_password: String
}

struct DeleteAccountRequest: Encodable {
    let password: String
}

struct LoginResponse: Decodable {
    let account: AccountResponse
    let session: SessionResponse
}

struct AuthMeResponse: Decodable {
    let account: AccountResponse
}

struct AccountResponse: Decodable {
    let account_id: String
    let email: String
    let email_verified: Bool
    let display_name: String?
}

struct DisplayNameRequest: Encodable {
    let display_name: String
}

struct SessionResponse: Decodable {
    let session_token: String
}

struct DeviceRegistrationResponse: Decodable {
    let device: RegisteredDevice
}

struct RegisteredDevice: Decodable {
    let device_id: String
    let device_name: String
    let notification_sound: String?
}

struct AccountDevicesResponse: Decodable {
    let devices: [AccountDevice]
}

struct AccountDevice: Decodable, Identifiable {
    let device_id: String
    let device_name: String
    let platform: String
    let last_ip_address: String
    let last_seen: String

    var id: String { device_id }

    var formattedLastSeen: String {
        guard let date = Self.parser.date(from: last_seen) else {
            return last_seen
        }
        return Self.formatter.string(from: date)
    }

    private static let parser = ISO8601DateFormatter()
    private static let formatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter
    }()
}

struct APIKeysResponse: Decodable {
    let api_keys: [ManagedAPIKey]
}

struct ManagedAPIKey: Decodable, Identifiable {
    let key_id: String
    let label: String
    let api_key: String
    let created_at: String?
    let last_used_at: String?

    var id: String { key_id }
}

struct CreateAPIKeyResponse: Decodable {
    let api_key: ManagedAPIKey
}

struct DeleteAPIKeysResponse: Decodable {
    let deleted: Bool
    let deleted_count: Int?
}

struct SignOutDevicesResponse: Decodable {
    let signed_out: Bool
    let deleted_devices: Int?
    let deleted_sessions: Int?
}

struct PasswordChangedResponse: Decodable {
    let password_changed: Bool
}

struct NotificationsResponse: Decodable {
    let notifications: [AccountNotification]
}

struct AccountNotification: Decodable, Identifiable {
    let notification_id: String
    let title: String
    let message: String
    let created_at: String
    let email: Bool
    let html: String?
    /// False until the notification list has been opened on any device.
    var seen: Bool

    var id: String { notification_id }

    var isEmailNotification: Bool { email }

    var displayTitle: String {
        guard isEmailNotification else {
            return title
        }
        var displayTitle = title
        if let addressRange = emailAddressBlockRange {
            displayTitle.removeSubrange(addressRange)
        }
        return Self.normalizedEmailHeaderText(displayTitle)
    }

    var emailSenderName: String {
        guard let addressRange = emailAddressBlockRange else {
            return Self.normalizedEmailHeaderText(title.components(separatedBy: ":").first ?? title)
        }
        let name = Self.normalizedEmailHeaderText(String(title[..<addressRange.lowerBound]))
        return name.isEmpty ? emailSenderAddress : name
    }

    var emailSenderAddress: String {
        guard let addressRange = emailAddressBlockRange else {
            return ""
        }
        let addressStart = title.index(after: addressRange.lowerBound)
        let addressEnd = title.index(before: addressRange.upperBound)
        return String(title[addressStart..<addressEnd])
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    var emailSubject: String {
        guard let colonIndex = title.firstIndex(of: ":") else {
            return title
        }
        let subject = title[title.index(after: colonIndex)...]
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return subject.isEmpty ? title : subject
    }

    var emailHTML: String {
        html ?? message
    }

    private var emailAddressBlockRange: Range<String.Index>? {
        guard
            let start = title.firstIndex(of: "<"),
            let end = title[start...].firstIndex(of: ">")
        else {
            return nil
        }
        return start..<title.index(after: end)
    }

    var formattedDate: String {
        guard let date = Self.parser.date(from: created_at) else {
            return created_at
        }
        return Self.formatter.string(from: date)
    }

    var parsedMessage: ParsedNotificationMessage {
        Self.parseMessage(message)
    }

    var messageLinks: [NotificationLink] {
        parsedMessage.links
    }

    var renderedMessage: AttributedString {
        (try? AttributedString(parsedMessage.attributedText, including: \.uiKit)) ?? AttributedString(message)
    }

    var accessibilityMessage: String {
        parsedMessage.accessibilityText
    }

    var plainTextForCopy: String {
        [title, message, Self.htmlSafeDateText(formattedDate)].joined(separator: "\n")
    }

    var richHTMLForCopy: String {
        """
        <div>
        <p><strong>\(Self.escapeHTML(title))</strong></p>
        <p>\(parsedMessage.htmlText)</p>
        <p>\(Self.escapeHTML(Self.htmlSafeDateText(formattedDate)))</p>
        </div>
        """
    }

    enum CodingKeys: String, CodingKey {
        case notification_id
        case title
        case message
        case created_at
        case email
        case html
        case seen
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        notification_id = try container.decode(String.self, forKey: .notification_id)
        title = try container.decode(String.self, forKey: .title)
        message = try container.decode(String.self, forKey: .message)
        created_at = try container.decode(String.self, forKey: .created_at)
        email = try container.decodeIfPresent(Bool.self, forKey: .email) ?? false
        html = try container.decodeIfPresent(String.self, forKey: .html)
        seen = try container.decodeIfPresent(Bool.self, forKey: .seen) ?? true
    }

    private static let parser = ISO8601DateFormatter()
    private static let linkDetector = try! NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue)
    private static let customLinkDetector = try! NSRegularExpression(
        pattern: #"\b[A-Za-z][A-Za-z0-9+\-.]*:[^\s<>()]+"#
    )
    private static let markdownLinkDetector = try! NSRegularExpression(
        pattern: #"\[([^\]]+)\]\(([^)]+)\)"#
    )
    private static let formatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter
    }()
    private static let hexDigits = CharacterSet(charactersIn: "0123456789ABCDEFabcdef")
    private static let markdownURLAllowedCharacters = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~:/?[]@!$&'()*+,;=")

    private static func normalizedLink(from text: String) -> URL? {
        let trimmed = text.trimmingCharacters(in: CharacterSet(charactersIn: ".,;:!?)]}>\"'"))
        return URL(string: trimmed)
    }

    private static func normalizedMarkdownLink(from text: String) -> URL? {
        let trimmed = text.trimmingCharacters(in: CharacterSet(charactersIn: ".,;:!?)]}>\"'"))
        return URL(string: percentEncodedMarkdownURL(trimmed))
    }

    private static func percentEncodedMarkdownURL(_ text: String) -> String {
        var encoded = ""
        var index = text.unicodeScalars.startIndex

        while index < text.unicodeScalars.endIndex {
            let scalar = text.unicodeScalars[index]
            if scalar == "%", let percentEncodedRange = validPercentEncodedRange(in: text.unicodeScalars, startingAt: index) {
                encoded += String(text.unicodeScalars[percentEncodedRange])
                index = percentEncodedRange.upperBound
            } else if markdownURLAllowedCharacters.contains(scalar) {
                encoded.unicodeScalars.append(scalar)
                index = text.unicodeScalars.index(after: index)
            } else {
                for byte in String(scalar).utf8 {
                    encoded += String(format: "%%%02X", byte)
                }
                index = text.unicodeScalars.index(after: index)
            }
        }

        return encoded
    }

    private static func validPercentEncodedRange(
        in scalars: String.UnicodeScalarView,
        startingAt index: String.UnicodeScalarView.Index
    ) -> Range<String.UnicodeScalarView.Index>? {
        let firstHexIndex = scalars.index(after: index)
        guard firstHexIndex < scalars.endIndex else {
            return nil
        }

        let secondHexIndex = scalars.index(after: firstHexIndex)
        guard secondHexIndex < scalars.endIndex else {
            return nil
        }

        guard hexDigits.contains(scalars[firstHexIndex]), hexDigits.contains(scalars[secondHexIndex]) else {
            return nil
        }

        return index..<scalars.index(after: secondHexIndex)
    }

    private static func parseMessage(_ message: String) -> ParsedNotificationMessage {
        let attributedText = NSMutableAttributedString()
        var accessibilityText = ""
        var htmlText = ""
        var links: [NotificationLink] = []
        var linkIndex = 0

        func appendPlainText(_ text: String) {
            guard !text.isEmpty else {
                return
            }
            let parsedPlainText = parsePlainTextSegment(text, linkIndex: &linkIndex)
            attributedText.append(parsedPlainText.attributedText)
            accessibilityText += parsedPlainText.accessibilityText
            htmlText += parsedPlainText.htmlText
            links.append(contentsOf: parsedPlainText.links)
        }

        let fullRange = NSRange(message.startIndex..<message.endIndex, in: message)
        let markdownMatches = markdownLinkDetector.matches(in: message, options: [], range: fullRange)
        var currentLocation = message.startIndex

        for match in markdownMatches {
            guard
                let fullMatchRange = Range(match.range, in: message),
                let titleRange = Range(match.range(at: 1), in: message),
                let urlRange = Range(match.range(at: 2), in: message)
            else {
                continue
            }

            appendPlainText(String(message[currentLocation..<fullMatchRange.lowerBound]))

            let linkTitle = String(message[titleRange])
            let rawURL = String(message[urlRange])
            if let url = normalizedMarkdownLink(from: rawURL) {
                let link = NotificationLink(
                    id: "link-\(linkIndex)",
                    displayText: linkTitle,
                    url: url
                )
                attributedText.append(link.attributedText)
                accessibilityText += link.accessibilityText
                htmlText += "<a href=\"\(escapeHTML(url.absoluteString))\">\(escapeHTML(linkTitle))</a>"
                links.append(link)
                linkIndex += 1
            } else {
                appendPlainText(String(message[fullMatchRange]))
            }

            currentLocation = fullMatchRange.upperBound
        }

        appendPlainText(String(message[currentLocation...]))

        return ParsedNotificationMessage(
            attributedText: attributedText.copy() as! NSAttributedString,
            accessibilityText: accessibilityText,
            htmlText: htmlText,
            links: links
        )
    }

    private static func parsePlainTextSegment(_ text: String, linkIndex: inout Int) -> ParsedNotificationMessage {
        let attributedText = NSMutableAttributedString()
        var accessibilityText = ""
        var htmlText = ""
        var links: [NotificationLink] = []

        let nsRange = NSRange(text.startIndex..<text.endIndex, in: text)
        let rawLinkMatches = (
            linkDetector.matches(in: text, options: [], range: nsRange).map(\.range) +
            customLinkDetector.matches(in: text, options: [], range: nsRange).map(\.range)
        )
        let normalizedMatches = normalizedLinkMatches(in: text, ranges: rawLinkMatches)

        var currentLocation = text.startIndex
        for match in normalizedMatches {
            guard let matchRange = Range(match.range, in: text) else {
                continue
            }

            let leadingText = String(text[currentLocation..<matchRange.lowerBound])
            if !leadingText.isEmpty {
                attributedText.append(NSAttributedString(string: leadingText))
                accessibilityText += leadingText
                htmlText += escapeHTML(leadingText).replacingOccurrences(of: "\n", with: "<br>")
            }

            let link = NotificationLink(
                id: "link-\(linkIndex)",
                displayText: match.text,
                url: match.url
            )
            attributedText.append(link.attributedText)
            accessibilityText += link.accessibilityText
            htmlText += "<a href=\"\(escapeHTML(match.url.absoluteString))\">\(escapeHTML(match.text))</a>"
            links.append(link)
            linkIndex += 1

            currentLocation = matchRange.upperBound
        }

        let trailingText = String(text[currentLocation...])
        if !trailingText.isEmpty {
            attributedText.append(NSAttributedString(string: trailingText))
            accessibilityText += trailingText
            htmlText += escapeHTML(trailingText).replacingOccurrences(of: "\n", with: "<br>")
        }

        return ParsedNotificationMessage(
            attributedText: attributedText.copy() as! NSAttributedString,
            accessibilityText: accessibilityText,
            htmlText: htmlText,
            links: links
        )
    }

    private static func normalizedLinkMatches(in text: String, ranges: [NSRange]) -> [NormalizedLinkMatch] {
        var matches: [NormalizedLinkMatch] = []
        var occupiedRanges: [NSRange] = []

        for range in ranges.sorted(by: { $0.location < $1.location }) {
            guard let stringRange = Range(range, in: text) else {
                continue
            }
            let rawText = String(text[stringRange])
            let trimmed = rawText.trimmingCharacters(in: CharacterSet(charactersIn: ".,;:!?)]}>\"'"))
            guard !trimmed.isEmpty, let url = URL(string: trimmed) else {
                continue
            }

            let adjustedRange = NSRange(location: range.location, length: (trimmed as NSString).length)
            guard !occupiedRanges.contains(where: { NSIntersectionRange($0, adjustedRange).length > 0 }) else {
                continue
            }

            occupiedRanges.append(adjustedRange)
            matches.append(
                NormalizedLinkMatch(
                    range: adjustedRange,
                    text: trimmed,
                    url: url
                )
            )
        }

        return matches
    }

    private static func escapeHTML(_ text: String) -> String {
        text
            .replacingOccurrences(of: "&", with: "&amp;")
            .replacingOccurrences(of: "<", with: "&lt;")
            .replacingOccurrences(of: ">", with: "&gt;")
            .replacingOccurrences(of: "\"", with: "&quot;")
    }

    private static func htmlSafeDateText(_ text: String) -> String {
        text
            .replacingOccurrences(of: "\u{202F}", with: " ")
            .replacingOccurrences(of: "\u{00A0}", with: " ")
    }

    private static func normalizedEmailHeaderText(_ text: String) -> String {
        text
            .replacingOccurrences(of: "  ", with: " ")
            .replacingOccurrences(of: " :", with: ":")
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

struct ParsedNotificationMessage {
    let attributedText: NSAttributedString
    let accessibilityText: String
    let htmlText: String
    let links: [NotificationLink]
}

struct NotificationLink: Identifiable, Hashable {
    let id: String
    let displayText: String
    let url: URL

    var accessibilityText: String {
        "Link: \(displayText)"
    }

    var linkSelectionTitle: String {
        "Link: \(displayText)"
    }

    var attributedText: NSAttributedString {
        NSAttributedString(
            string: displayText,
            attributes: [
                .link: url,
                .foregroundColor: UIColor.link,
                .underlineStyle: NSUnderlineStyle.single.rawValue,
            ]
        )
    }
}

private struct NormalizedLinkMatch {
    let range: NSRange
    let text: String
    let url: URL
}

struct EmptyResponse: Decodable {}

struct AcceptedResponse: Decodable {
    let accepted: Bool
}

struct HealthResponse: Decodable {
    let status: String
}

struct ClearNotificationsResponse: Decodable {
    let deleted_count: Int
}

struct ServerErrorResponse: Decodable {
    let detail: ServerErrorDetail

    var detailText: String {
        switch detail {
        case let .text(text):
            return text
        case let .object(object):
            return object.message ?? "Server error"
        }
    }
}

enum ServerErrorDetail: Decodable {
    case text(String)
    case object(ServerErrorObject)

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let text = try? container.decode(String.self) {
            self = .text(text)
        } else {
            self = .object(try container.decode(ServerErrorObject.self))
        }
    }
}

struct ServerErrorObject: Decodable {
    let message: String?
}

struct SoundOption: Identifiable, Hashable {
    let id: String
    let title: String
}

struct PendingSoundImport: Identifiable, Hashable {
    let url: URL

    var id: String {
        fileName
    }

    var fileName: String {
        url.lastPathComponent
    }

    var displayName: String {
        url.deletingPathExtension().lastPathComponent
    }
}

enum AuthScreenMode: String, CaseIterable, Identifiable {
    case signIn = "Sign In"
    case createAccount = "Create Account"
    case forgotPassword = "Forgot Password"

    var id: String { rawValue }
}

struct ContentView: View {
    @StateObject private var model = MGNotifyModel.shared
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.isSearching) private var isSearching
    @FocusState private var isNotificationSearchFocused: Bool
    @State private var showingSettings = false
    @State private var showingSounds = false
    @State private var showingAddSounds = false
    @State private var copyTargetNotification: AccountNotification?
    @State private var selectedEmailNotification: AccountNotification?
    @State private var showingClearNotificationsConfirmation = false
    @State private var notificationSearchText = ""
    @State private var showPassword = false

    var body: some View {
        NavigationStack {
            Group {
                if !model.hasInternetConnection {
                    offlineView
                } else if model.isVerifyingStoredSession {
                    ProgressView()
                } else if model.remoteSignOutNotice {
                    remoteSignOutView
                } else if model.accountVerificationFailed {
                    verificationFailedView
                } else if model.isAuthenticated, model.emailVerificationRequired {
                    emailVerificationRequiredView
                } else if model.isAuthenticated {
                    signedInView
                } else {
                    loginView
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(backgroundGradient)
            .navigationTitle("MG Notify")
            .task {
                await model.performLaunchChecksIfNeeded()
                openAddSoundsIfPending()
                openSoundsIfPending()
            }
            .onChange(of: scenePhase) { _, newPhase in
                if newPhase == .active {
                    model.refreshPermissionStatus()
                    model.reloadImportedSounds()
                    openAddSoundsIfPending()
                    openSoundsIfPending()
                    // A sound shared to the share extension on its own became
                    // the notification sound; tell the server.
                    if model.consumePendingSelectedSoundSync() {
                        Task {
                            await model.saveSelectedSound()
                        }
                    }
                    Task {
                        await model.refreshServerAndSession()
                    }
                    if isNotificationListInView {
                        model.scheduleMarkSeen(syncBadge: true)
                    }
                } else {
                    model.stopLiveUpdates()
                    if newPhase == .background {
                        model.forgetNewMarkers()
                    }
                }
            }
            .onChange(of: model.hasInternetConnection) { _, hasInternetConnection in
                if hasInternetConnection, scenePhase == .active {
                    Task {
                        await model.retryServerConnection()
                    }
                } else if !hasInternetConnection {
                    model.stopLiveUpdates()
                }
            }
            .navigationDestination(isPresented: $showingSettings) {
                SettingsView(model: model)
            }
            .navigationDestination(isPresented: $showingAddSounds) {
                AddSoundsView(model: model) {
                    showingAddSounds = false
                    showingSounds = true
                }
            }
            .navigationDestination(isPresented: $showingSounds) {
                SoundsView(model: model)
            }
        }
        .id(model.navigationResetToken)
        .onChange(of: model.navigationResetToken) { _, _ in
            showingSettings = false
            showingSounds = false
            showingAddSounds = false
            copyTargetNotification = nil
            selectedEmailNotification = nil
        }
        .onChange(of: model.isAuthenticated) { _, isAuthenticated in
            if isAuthenticated {
                showPassword = false
            }
        }
        .onChange(of: model.authErrorMessage) { _, message in
            announceBannerMessage(message)
        }
        .onChange(of: model.verificationMessage) { _, message in
            announceBannerMessage(message)
        }
        .onOpenURL { url in
            handleOpenURL(url)
        }
    }

    private var loginView: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Text(model.isAddingAccount ? "Add Account" : "Welcome to MG Notify")
                    .font(.largeTitle.bold())
                    .frame(maxWidth: .infinity, alignment: .leading)
                VStack(alignment: .leading, spacing: 14) {
                    Picker("Account", selection: $model.authScreenMode) {
                        ForEach(AuthScreenMode.allCases) { mode in
                            Text(mode.rawValue).tag(mode)
                        }
                    }
                    .pickerStyle(.segmented)
                    .onChange(of: model.authScreenMode) { _, _ in
                        model.authErrorMessage = ""
                    }
                    TextField("Email", text: $model.email)
                        .textInputAutocapitalization(.never)
                        .keyboardType(.emailAddress)
                        .autocorrectionDisabled()
                        .textFieldStyle(.roundedBorder)
                    if model.authScreenMode != .forgotPassword {
                        if showPassword {
                            TextField("Password", text: $model.password)
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled()
                                .textFieldStyle(.roundedBorder)
                        } else {
                            SecureField("Password", text: $model.password)
                                .textFieldStyle(.roundedBorder)
                        }
                        Toggle("Show Password", isOn: $showPassword)
                        if model.authScreenMode == .createAccount {
                            Text("Your password must be at least 8 characters.")
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        }
                    }
                    if !model.authErrorMessage.isEmpty {
                        Text(model.authErrorMessage)
                            .font(.footnote)
                            .foregroundStyle(.red)
                    }
                    switch model.authScreenMode {
                    case .signIn:
                        actionButton(
                            "Sign In",
                            isEnabled: !model.email.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !model.password.isEmpty
                        ) {
                            Task {
                                await model.login()
                            }
                        }
                    case .createAccount:
                        actionButton(
                            "Create Account",
                            isEnabled: !model.email.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !model.password.isEmpty
                        ) {
                            Task {
                                await model.registerAccount()
                            }
                        }
                    case .forgotPassword:
                        actionButton(
                            "Send Reset Email",
                            isEnabled: !model.email.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        ) {
                            Task {
                                await model.requestPasswordReset()
                            }
                        }
                    }
                }
                .padding(20)
                .background(cardBackground)
                if model.isAddingAccount {
                    Button("Cancel") {
                        Task {
                            await model.cancelAddingAccount()
                        }
                    }
                    .frame(maxWidth: .infinity)
                    .disabled(model.isBusy)
                }
            }
            .padding(20)
        }
    }

    private var offlineView: some View {
        VStack(spacing: 18) {
            Text("An internet connection is required to use MG Notify. Please connect to the internet and try again.")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
        }
        .padding(24)
    }

    private var signedInView: some View {
        List {
            if model.isNotificationPermissionDenied, !isNotificationsSearchActive {
                notificationPermissionBannerSection
            }

            if !isNotificationsSearchActive {
                Section {
                    Menu {
                        ForEach(model.savedAccounts) { account in
                            Button {
                                guard !model.isActiveAccount(account) else {
                                    return
                                }
                                Task {
                                    await model.switchToAccount(account)
                                }
                            } label: {
                                if model.isActiveAccount(account) {
                                    Label(account.name, systemImage: "checkmark")
                                } else {
                                    Text(account.name)
                                }
                                if !account.displayName.isEmpty {
                                    Text(account.email)
                                }
                            }
                        }
                        Divider()
                        Button {
                            model.startAddingAccount()
                        } label: {
                            Label("Add Account", systemImage: "plus")
                        }
                    } label: {
                        HStack(spacing: 8) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text("Account")
                                    .font(.title2.weight(.semibold))
                                Text(model.accountName)
                                    .font(.subheadline)
                                    .foregroundStyle(.secondary)
                                    .multilineTextAlignment(.leading)
                            }
                            .accessibilityElement(children: .ignore)
                            .accessibilityLabel("Account")
                            .accessibilityValue(model.accountName)
                            Spacer(minLength: 0)
                            Image(systemName: "chevron.up.chevron.down")
                                .font(.footnote.weight(.semibold))
                                .foregroundStyle(.secondary)
                        }
                        .foregroundStyle(.primary)
                        .padding(.vertical, 4)
                    }
                    .accessibilityHint("Switch accounts or add another account")
                }
            }

            if model.notifications.isEmpty {
                Section {
                    Text("No notifications yet.")
                        .foregroundStyle(.secondary)
                }
            } else {
                if !isNotificationsSearchActive {
                    notificationCountSection
                }
                notificationListSection
            }
        }
        .scrollContentBackground(.hidden)
        .modifier(
            SearchBarModifier(
                isEnabled: !model.notifications.isEmpty,
                searchText: $notificationSearchText,
                isFocused: $isNotificationSearchFocused,
                prompt: "Search notifications"
            )
        )
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button {
                    showingSettings = true
                } label: {
                    Image(systemName: "gearshape")
                }
                .accessibilityLabel("Settings")
            }

            if !model.notifications.isEmpty {
                ToolbarItem(placement: .topBarTrailing) {
                    Button(role: .destructive) {
                        showingClearNotificationsConfirmation = true
                    } label: {
                        Image(systemName: "trash")
                    }
                    .accessibilityLabel("Delete All Notifications")
                }
            }
        }
        .navigationBarTitleDisplayMode(.inline)
        .sheet(item: $selectedEmailNotification) { notification in
            EmailNotificationDetailView(notification: notification)
                .onAppear { model.markEmailSeen(notification.notification_id) }
        }
        // A tapped email notification opens straight to the email, as soon
        // as it's in the list (after switching account or loading, if need be).
        .onAppear(perform: openTappedEmail)
        .onReceive(model.$emailNotificationToOpen) { _ in DispatchQueue.main.async(execute: openTappedEmail) }
        .onReceive(model.$notifications) { _ in DispatchQueue.main.async(execute: openTappedEmail) }
        .confirmationDialog(
            "Copy Notification",
            isPresented: Binding(
                get: { copyTargetNotification != nil },
                set: { isPresented in
                    if !isPresented {
                        copyTargetNotification = nil
                    }
                }
            ),
            titleVisibility: Visibility.visible
        ) {
            if let notification = copyTargetNotification {
                Button("Copy Rich Text") {
                    copyRichText(for: notification)
                }
                Button("Copy Plain Text") {
                    copyPlainText(for: notification)
                }
            }
            Button("Cancel", role: .cancel) {}
        }
        .confirmationDialog(
            "Are you sure you wish to delete all your notifications?",
            isPresented: $showingClearNotificationsConfirmation,
            titleVisibility: Visibility.visible
        ) {
            Button("Delete All Notifications", role: .destructive) {
                Task {
                    await model.clearNotifications()
                }
            }
            Button("Cancel", role: .cancel) {}
        }
        .task {
            await model.fetchNotifications()
        }
        .task(id: model.notificationsRefreshToken) {
            await model.fetchNotifications()
        }
        .onChange(of: model.notifications.count) { _, count in
            if count == 0 {
                notificationSearchText = ""
            }
        }
        // The list is on screen: what's unread here counts as seen.
        .onChange(of: model.notifications.filter { !$0.seen }.count, initial: true) { _, unread in
            if unread > 0, isNotificationListInView {
                model.scheduleMarkSeen()
            }
        }
        .onChange(of: isNotificationListInView) { _, inView in
            if inView {
                model.scheduleMarkSeen()
            } else {
                model.cancelMarkSeen()
            }
        }
    }

    private func openTappedEmail() {
        guard
            let id = model.emailNotificationToOpen,
            let notification = model.notifications.first(where: { $0.notification_id == id })
        else {
            return
        }
        model.emailNotificationToOpen = nil
        showingSettings = false
        showingSounds = false
        showingAddSounds = false
        copyTargetNotification = nil
        selectedEmailNotification = notification
    }

    /// The notification list is what's on screen (nothing pushed or presented over it).
    private var isNotificationListInView: Bool {
        model.isAuthenticated && !model.emailVerificationRequired && !showingSettings && !showingSounds
            && !showingAddSounds && selectedEmailNotification == nil
    }

    private var filteredNotifications: [AccountNotification] {
        let query = notificationSearchText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else {
            return model.notifications
        }
        return model.notifications.filter { notification in
            notification.title.localizedCaseInsensitiveContains(query) ||
                notification.message.localizedCaseInsensitiveContains(query) ||
                notification.formattedDate.localizedCaseInsensitiveContains(query)
        }
    }

    private var notificationCountText: String {
        let count = model.notifications.count
        let noun = count == 1 ? "notification." : "notifications."
        return "You currently have \(count) \(noun)"
    }

    private var isNotificationsSearchActive: Bool {
        isSearching || isNotificationSearchFocused || !notificationSearchText.isEmpty
    }

    private var notificationPermissionBannerSection: some View {
        Section {
            HStack(alignment: .center, spacing: 12) {
                Text("You did not allow the notification permission, so you will not get push notifications. Go to Settings to enable.")
                    .font(.footnote)
                    .foregroundStyle(.primary)
                    .fixedSize(horizontal: false, vertical: true)

                Spacer(minLength: 8)

                Button("Go to Settings") {
                    model.openNotificationSettings()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .accessibilityHidden(true)
            }
            .padding(.vertical, 4)
            .accessibilityElement(children: .combine)
            .accessibilityLabel("You did not allow the notification permission, so you will not get push notifications. Go to Settings to enable.")
            .accessibilityAction(named: Text("Go to Settings")) {
                model.openNotificationSettings()
            }
        }
    }

    private var notificationCountSection: some View {
        Section {
            Text(notificationCountText)
                .foregroundStyle(.secondary)
        }
    }

    private var notificationListSection: some View {
        Group {
            if isNotificationsSearchActive {
                Section {
                    if filteredNotifications.isEmpty {
                        Text("No matching notifications.")
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(filteredNotifications) { notification in
                            notificationRow(for: notification)
                        }
                    }
                }
            } else {
                Section("Notifications") {
                    if filteredNotifications.isEmpty {
                        Text("No matching notifications.")
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(filteredNotifications) { notification in
                            notificationRow(for: notification)
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func notificationRow(for notification: AccountNotification) -> some View {
        if notification.isEmailNotification {
            Button {
                selectedEmailNotification = notification
            } label: {
                NotificationRow(notification: notification, isNew: model.newNotificationIDs.contains(notification.notification_id))
            }
            .buttonStyle(.plain)
            .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                deleteNotificationButton(for: notification)
            }
        } else {
            NotificationRow(notification: notification, isNew: model.newNotificationIDs.contains(notification.notification_id))
                .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                    Button {
                        copyTargetNotification = notification
                    } label: {
                        Label("Copy", systemImage: "doc.on.doc")
                    }
                    deleteNotificationButton(for: notification)
                }
        }
    }

    private func deleteNotificationButton(for notification: AccountNotification) -> some View {
        Button(role: .destructive) {
            Task {
                await model.deleteNotification(notificationID: notification.notification_id)
            }
        } label: {
            Label("Delete", systemImage: "trash")
        }
    }

    private func announceBannerMessage(_ message: String) {
        guard !message.isEmpty else {
            return
        }
        UIAccessibility.post(notification: .announcement, argument: message)
    }

    private func handleOpenURL(_ url: URL) {
        guard url.scheme == "mgnotify" else {
            return
        }

        if url.host == "add-sounds" {
            openAddSoundsFromExternalImport()
        } else if url.host == "sounds" {
            openSoundsFromExternalImport()
        }
    }

    private func openAddSoundsIfPending() {
        guard model.consumePendingOpenAddSounds() else {
            return
        }
        openAddSoundsFromExternalImport()
    }

    private func openAddSoundsFromExternalImport() {
        showingSettings = false
        showingSounds = false
        showingAddSounds = false
        Task { @MainActor in
            await Task.yield()
            showingAddSounds = true
        }
    }

    private func openSoundsIfPending() {
        guard model.consumePendingOpenSounds() else {
            return
        }
        openSoundsFromExternalImport()
    }

    private func openSoundsFromExternalImport() {
        model.reloadImportedSounds()
        showingSettings = false
        showingSounds = false
        Task { @MainActor in
            await Task.yield()
            showingSounds = true
        }
        if model.consumePendingSelectedSoundSync() {
            Task {
                await model.saveSelectedSound()
            }
        }
    }

    private var emailVerificationRequiredView: some View {
        VStack(spacing: 20) {
            Text("Verify your email")
                .font(.title2.weight(.semibold))
            Text("You need to verify your email in order to use MG Notify.")
                .multilineTextAlignment(.center)
            Text("Check the inbox for \(model.email) and follow the verification link.")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            if !model.verificationMessage.isEmpty {
                Text(model.verificationMessage)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            if !model.authErrorMessage.isEmpty {
                Text(model.authErrorMessage)
                    .font(.footnote)
                    .foregroundStyle(.red)
                    .multilineTextAlignment(.center)
            }
            actionButton("Resend Verification Email") {
                Task {
                    await model.resendVerificationEmail()
                }
            }
            Button("Sign Out") {
                Task {
                    await model.logout()
                }
            }
            .buttonStyle(.plain)
        }
        .padding(24)
    }

    private func copyRichText(for notification: AccountNotification) {
        UIPasteboard.general.setItems([
            [
                UTType.html.identifier: notification.richHTMLForCopy,
                UTType.utf8PlainText.identifier: notification.plainTextForCopy,
            ],
        ])
        copyTargetNotification = nil
    }

    private func copyPlainText(for notification: AccountNotification) {
        UIPasteboard.general.string = notification.plainTextForCopy
        copyTargetNotification = nil
    }

    private var verificationFailedView: some View {
        VStack(spacing: 20) {
            Text("Server unreachable")
                .font(.title2.weight(.semibold))
            Text("MG Notify could not reach the server. Please try again.")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            actionButton("Try Again") {
                Task {
                    await model.retryServerConnection()
                }
            }
        }
        .padding(24)
    }

    private var remoteSignOutView: some View {
        VStack(spacing: 20) {
            Text("You have been signed out.")
                .font(.title2.weight(.semibold))
            Text("Please log in again.")
                .foregroundStyle(.secondary)
            actionButton("OK") {
                model.forgetLocalAccountData()
            }
        }
        .padding(24)
    }

    private func headerCard(title: String, subtitle: String) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(title)
                .font(.largeTitle.bold())
            Text(subtitle)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(20)
        .background(cardBackground)
    }

    private var backgroundGradient: some View {
        LinearGradient(
            colors: [
                Color(red: 0.95, green: 0.97, blue: 0.99),
                Color(red: 0.88, green: 0.92, blue: 0.96),
                Color(red: 0.98, green: 0.94, blue: 0.90),
            ],
            startPoint: .topLeading,
            endPoint: .bottomTrailing
        )
        .ignoresSafeArea()
    }

    private var cardBackground: some ShapeStyle {
        .regularMaterial
    }

    private func actionButton(_ title: String, isEnabled: Bool = true, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack {
                Text(title)
                if model.isBusy {
                    ProgressView()
                        .tint(.white)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 12)
            .background(Color.accentColor, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .foregroundStyle(.white)
        }
        .buttonStyle(.plain)
        .disabled(model.isBusy || !isEnabled)
    }
}

struct NotificationRow: View {
    let notification: AccountNotification
    /// Unread when this visit to the app began.
    var isNew = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text(notification.displayTitle)
                    .font(.headline)
                if isNew {
                    Text("New")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 2)
                        .background(Color.accentColor, in: Capsule())
                }
            }
            if !notification.isEmailNotification {
                Text(notification.renderedMessage)
                    .font(.body)
            }
            Text(notification.formattedDate)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, 4)
        .tint(.blue)
        .accessibilityElement(children: .combine)
    }
}

struct EmailNotificationDetailView: View {
    @Environment(\.dismiss) private var dismiss
    let notification: AccountNotification

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                VStack(alignment: .leading, spacing: 12) {
                    EmailNotificationDetailRow(
                        label: "Sender Name",
                        value: notification.emailSenderName
                    )
                    EmailNotificationDetailRow(
                        label: "Sender Address",
                        value: notification.emailSenderAddress.isEmpty ? "Unknown" : notification.emailSenderAddress
                    )
                    EmailNotificationDetailRow(
                        label: "Subject",
                        value: notification.emailSubject
                    )
                    EmailNotificationDetailRow(
                        label: "Date",
                        value: notification.formattedDate
                    )
                }
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color(.secondarySystemGroupedBackground))
                .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                .padding()

                Divider()

                EmailNotificationWebView(html: notification.emailHTML)
            }
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button {
                        dismiss()
                    } label: {
                        Image(systemName: "xmark")
                    }
                    .accessibilityLabel("Close")
                }

                ToolbarItem(placement: .principal) {
                    Text(notification.emailSubject)
                        .font(.headline)
                        .lineLimit(1)
                        .accessibilityAddTraits(.isHeader)
                }
            }
        }
    }
}

private struct EmailNotificationDetailRow: View {
    let label: String
    let value: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(value)
                .font(.body)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
        .accessibilityValue(value)
    }
}

struct EmailNotificationWebView: UIViewRepresentable {
    let html: String

    func makeUIView(context: Context) -> WKWebView {
        let webView = WKWebView()
        webView.navigationDelegate = context.coordinator
        webView.isOpaque = false
        webView.backgroundColor = .systemBackground
        webView.scrollView.backgroundColor = .systemBackground
        return webView
    }

    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func updateUIView(_ webView: WKWebView, context: Context) {
        webView.loadHTMLString(htmlDocument, baseURL: nil)
    }

    final class Coordinator: NSObject, WKNavigationDelegate {
        func webView(
            _ webView: WKWebView,
            decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            guard
                navigationAction.navigationType == .linkActivated,
                let url = navigationAction.request.url
            else {
                decisionHandler(.allow)
                return
            }

            UIApplication.shared.open(url)
            decisionHandler(.cancel)
        }
    }

    private var htmlDocument: String {
        let trimmedHTML = html.trimmingCharacters(in: .whitespacesAndNewlines)
        let lowercasedHTML = trimmedHTML.lowercased()
        if lowercasedHTML.contains("<html") || lowercasedHTML.contains("<!doctype html") {
            return trimmedHTML
        }

        return """
        <!doctype html>
        <html>
        <head>
        <meta charset="utf-8">
        <meta name="viewport" content="width=device-width, initial-scale=1.0, viewport-fit=cover">
        <style>
        html {
            -webkit-text-size-adjust: 100%;
            background: transparent;
        }
        body {
            margin: 16px;
            overflow-wrap: anywhere;
            background: transparent;
            color: CanvasText;
            font: -apple-system-body;
        }
        img, video, iframe {
            max-width: 100%;
            height: auto;
        }
        table {
            max-width: 100%;
        }
        pre {
            white-space: pre-wrap;
        }
        </style>
        </head>
        <body>
        \(trimmedHTML)
        </body>
        </html>
        """
    }
}

struct SettingsView: View {
    @ObservedObject var model: MGNotifyModel
    @State private var showingSignOutConfirmation = false

    var body: some View {
        List {
            Section {
                NavigationLink {
                    SoundsView(model: model)
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Notification Sound")
                        Text(model.selectedSoundTitle())
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                }

                NavigationLink {
                    AccountsView(model: model)
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Accounts")
                        Text(model.accountName)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                }

                NavigationLink("Account Settings") {
                    AccountSettingsView(model: model)
                }

                Button("Sign Out", role: .destructive) {
                    showingSignOutConfirmation = true
                }

                NavigationLink("About") {
                    AboutView()
                }
            }
        }
        .navigationTitle("Settings")
        .confirmationDialog(
            "Are you sure you wish to sign out?",
            isPresented: $showingSignOutConfirmation,
            titleVisibility: Visibility.visible
        ) {
            Button("Sign Out", role: .destructive) {
                Task {
                    await model.logout()
                }
            }
            Button("Cancel", role: .cancel) {}
        }
    }
}

struct AccountsView: View {
    @ObservedObject var model: MGNotifyModel
    @State private var accountPendingSignOut: SavedAccount?

    var body: some View {
        List {
            Section {
                ForEach(model.savedAccounts) { account in
                    Button {
                        guard !model.isActiveAccount(account) else {
                            return
                        }
                        Task {
                            await model.switchToAccount(account)
                        }
                    } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(account.name)
                                    .foregroundStyle(.primary)
                                if !account.displayName.isEmpty {
                                    Text(account.email)
                                        .font(.footnote)
                                        .foregroundStyle(.secondary)
                                }
                            }
                            Spacer()
                            if model.isActiveAccount(account) {
                                Image(systemName: "checkmark")
                                    .foregroundStyle(.tint)
                            }
                        }
                    }
                    .accessibilityAddTraits(model.isActiveAccount(account) ? .isSelected : [])
                    .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                        Button(role: .destructive) {
                            accountPendingSignOut = account
                        } label: {
                            Label("Sign Out", systemImage: "rectangle.portrait.and.arrow.right")
                        }
                    }
                    .accessibilityAction(named: Text("Sign Out")) {
                        accountPendingSignOut = account
                    }
                }
            } footer: {
                Text("This device receives notifications for every account you are signed in to. Tap an account to switch to it.")
            }

            Section {
                Button {
                    model.startAddingAccount()
                } label: {
                    Label("Add Account", systemImage: "plus")
                }
            }
        }
        .navigationTitle("Accounts")
        .confirmationDialog(
            "Are you sure you wish to sign out of \(accountPendingSignOut?.email ?? "this account")?",
            isPresented: Binding(
                get: { accountPendingSignOut != nil },
                set: { isPresented in
                    if !isPresented {
                        accountPendingSignOut = nil
                    }
                }
            ),
            titleVisibility: Visibility.visible
        ) {
            if let account = accountPendingSignOut {
                Button("Sign Out", role: .destructive) {
                    Task {
                        await model.signOutSavedAccount(account)
                    }
                }
            }
            Button("Cancel", role: .cancel) {}
        }
    }
}

struct AboutView: View {
    @Environment(\.openURL) private var openURL

    private var appVersionText: String {
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        let build = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String

        switch (version, build) {
        case let (version?, build?) where !version.isEmpty && !build.isEmpty:
            return "Version \(version) Build \(build)"
        case let (version?, _) where !version.isEmpty:
            return "Version \(version) Build unavailable"
        case let (_, build?) where !build.isEmpty:
            return "Version unavailable Build \(build)"
        default:
            return "Version unavailable Build unavailable"
        }
    }

    var body: some View {
        List {
            Section {
                VStack(alignment: .leading, spacing: 6) {
                    Text("MG Notify")
                        .font(.headline)
                    Text(appVersionText)
                        .foregroundStyle(.secondary)
                }
                .padding(.vertical, 4)
            }

            Section {
                Button("Open Web Portal") {
                    if let portalURL = URL(string: "https://notify.mad-gamer.com") {
                        openURL(portalURL)
                    }
                }

                Button("Email Developer") {
                    if let emailURL = URL(string: "mailto:mattdovi@icloud.com") {
                        openURL(emailURL)
                    }
                }
            }
        }
        .navigationTitle("About")
    }
}

struct AccountSettingsView: View {
    @Environment(\.scenePhase) private var scenePhase
    @ObservedObject var model: MGNotifyModel
    @State private var devices: [AccountDevice] = []
    @State private var apiKeys: [ManagedAPIKey] = []
    @State private var apiKeyLabel = "Default"
    @State private var showingCreateAPIKeyPrompt = false
    @State private var newEmail = ""
    @State private var emailPassword = ""
    @State private var currentPassword = ""
    @State private var newPassword = ""
    @State private var deletePassword = ""
    @State private var errorMessage = ""
    @State private var isWorking = false
    @State private var showDevicesSection = false
    @State private var showAPIKeysSection = false
    @State private var showChangeEmailSection = false
    @State private var showDisplayNameSection = false
    @State private var displayNameText = ""
    @State private var showChangePasswordSection = false
    @State private var showDeleteAccountSection = false
    @State private var showEmailPassword = false
    @State private var showChangePasswordFields = false
    @State private var showDeletePassword = false
    @State private var showingSignOutOtherDevicesConfirmation = false
    @State private var showingDeleteAllAPIKeysConfirmation = false

    var body: some View {
        List {
            if !errorMessage.isEmpty {
                Section {
                    Text(errorMessage)
                        .foregroundStyle(.red)
                }
            }

            Section {
                DisclosureGroup("Devices", isExpanded: $showDevicesSection) {
                    if otherDevices.isEmpty {
                        Text("No other devices registered.")
                            .foregroundStyle(.secondary)
                    } else {
                        Button("Sign Out Other Devices", role: .destructive) {
                            showingSignOutOtherDevicesConfirmation = true
                        }

                        ForEach(otherDevices) { device in
                            VStack(alignment: .leading, spacing: 4) {
                                Text(device.device_name)
                                    .font(.headline)
                                Text("\(device.platform) • \(device.last_ip_address)")
                                    .font(.subheadline)
                                    .foregroundStyle(.secondary)
                                Text(device.formattedLastSeen)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            .accessibilityElement(children: .combine)
                            .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                                Button(role: .destructive) {
                                    Task {
                                        await performAction {
                                            try await model.signOutManagedDevice(deviceID: device.device_id)
                                            devices = devices.filter { $0.device_id != device.device_id }
                                        }
                                    }
                                } label: {
                                    Label("Sign Out", systemImage: "rectangle.portrait.and.arrow.right")
                                }
                            }
                        }
                    }
                }
            }

            Section {
                DisclosureGroup("API Keys", isExpanded: $showAPIKeysSection) {
                    Button("Create API Key") {
                        apiKeyLabel = "Default"
                        showingCreateAPIKeyPrompt = true
                    }

                    if apiKeys.isEmpty {
                        Text("No API keys yet.")
                            .foregroundStyle(.secondary)
                    } else {
                        Button("Delete All API Keys", role: .destructive) {
                            showingDeleteAllAPIKeysConfirmation = true
                        }

                        ForEach(apiKeys) { apiKey in
                            VStack(alignment: .leading, spacing: 6) {
                                Text(apiKey.label)
                                    .font(.headline)
                                Text(apiKey.api_key)
                                    .font(.footnote.monospaced())
                                    .textSelection(.enabled)
                            }
                            .accessibilityElement(children: .combine)
                            .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                                Button {
                                    UIPasteboard.general.string = apiKey.api_key
                                } label: {
                                    Label("Copy", systemImage: "doc.on.doc")
                                }
                                Button(role: .destructive) {
                                    Task {
                                        await performAction {
                                            try await model.deleteManagedAPIKey(keyID: apiKey.key_id)
                                            apiKeys = apiKeys.filter { $0.key_id != apiKey.key_id }
                                        }
                                    }
                                } label: {
                                    Label("Delete", systemImage: "trash")
                                }
                            }
                        }
                    }
                }
            }

            Section {
                DisclosureGroup("Display Name", isExpanded: $showDisplayNameSection) {
                    TextField("Display name (optional)", text: $displayNameText)
                        .textInputAutocapitalization(.words)
                        .onChange(of: displayNameText) { _, text in
                            if text.count > 60 {
                                displayNameText = String(text.prefix(60))
                            }
                        }
                    Text("Shown instead of your email in the account menu and in announcements.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    Button("Save Display Name") {
                        Task {
                            await performAction {
                                try await model.setDisplayName(displayNameText)
                            }
                        }
                    }
                    .disabled(displayNameText.trimmingCharacters(in: .whitespacesAndNewlines) == model.displayName)
                }
            }

            Section {
                DisclosureGroup("Change Email", isExpanded: $showChangeEmailSection) {
                    TextField("New email", text: $newEmail)
                        .textInputAutocapitalization(.never)
                        .keyboardType(.emailAddress)
                        .autocorrectionDisabled()
                    if showEmailPassword {
                        TextField("Current password", text: $emailPassword)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    } else {
                        SecureField("Current password", text: $emailPassword)
                    }
                    Toggle("Show Password", isOn: $showEmailPassword)
                    Button("Change Email") {
                        Task {
                            await performAction {
                                try await model.changeAccountEmail(to: newEmail, password: emailPassword)
                            }
                        }
                    }
                    .disabled(newEmail.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || emailPassword.isEmpty)
                }
            }

            Section {
                DisclosureGroup("Change Password", isExpanded: $showChangePasswordSection) {
                    if showChangePasswordFields {
                        TextField("Current password", text: $currentPassword)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    } else {
                        SecureField("Current password", text: $currentPassword)
                    }
                    if showChangePasswordFields {
                        TextField("New password", text: $newPassword)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    } else {
                        SecureField("New password", text: $newPassword)
                    }
                    Toggle("Show Password", isOn: $showChangePasswordFields)
                    Button("Change Password") {
                        Task {
                            await performAction {
                                try await model.changeAccountPassword(currentPassword: currentPassword, newPassword: newPassword)
                            }
                        }
                    }
                    .disabled(currentPassword.isEmpty || newPassword.isEmpty)
                }
            }

            Section {
                DisclosureGroup("Delete Account", isExpanded: $showDeleteAccountSection) {
                    if showDeletePassword {
                        TextField("Confirm password", text: $deletePassword)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    } else {
                        SecureField("Confirm password", text: $deletePassword)
                    }
                    Toggle("Show Password", isOn: $showDeletePassword)
                    Button("Delete Account", role: .destructive) {
                        Task {
                            await performAction {
                                try await model.deleteAccount(password: deletePassword)
                            }
                        }
                    }
                    .disabled(deletePassword.isEmpty)
                }
            }
        }
        .navigationTitle("Account Settings")
        .onAppear {
            displayNameText = model.displayName
        }
        .alert("Create API Key", isPresented: $showingCreateAPIKeyPrompt) {
            TextField("Key label", text: $apiKeyLabel)
            Button("Cancel", role: .cancel) {}
            Button("Create") {
                Task {
                    await performAction {
                        let createdKey = try await model.createManagedAPIKey(
                            label: apiKeyLabel.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "Default" : apiKeyLabel
                        )
                        apiKeys.insert(createdKey, at: 0)
                    }
                }
            }
        } message: {
            Text("Enter a label for this API key.")
        }
        .confirmationDialog(
            "Are you sure you wish to sign out other devices?",
            isPresented: $showingSignOutOtherDevicesConfirmation,
            titleVisibility: Visibility.visible
        ) {
            Button("Sign Out Other Devices", role: .destructive) {
                Task {
                    await performAction {
                        try await model.signOutOtherManagedDevices()
                        devices = devices.filter { $0.device_id == model.deviceID }
                    }
                }
            }
            Button("Cancel", role: .cancel) {}
        }
        .confirmationDialog(
            "Are you sure you wish to delete all API keys?",
            isPresented: $showingDeleteAllAPIKeysConfirmation,
            titleVisibility: Visibility.visible
        ) {
            Button("Delete All API Keys", role: .destructive) {
                Task {
                    await performAction {
                        try await model.deleteAllManagedAPIKeys()
                        apiKeys = []
                    }
                }
            }
            Button("Cancel", role: .cancel) {}
        }
        .task {
            await reloadData()
        }
        .task(id: scenePhase) {
            guard scenePhase == .active else {
                return
            }
            await reloadData()
        }
        .task(id: model.devicesRefreshToken) {
            await reloadDevices()
        }
        .task(id: model.apiKeysRefreshToken) {
            await reloadAPIKeys()
        }
        .onChange(of: errorMessage) { _, message in
            guard !message.isEmpty else {
                return
            }
            UIAccessibility.post(notification: .announcement, argument: message)
        }
    }

    private func reloadData() async {
        guard !isWorking else {
            return
        }
        isWorking = true
        defer { isWorking = false }
        do {
            async let loadedDevices = model.loadAccountDevices()
            async let loadedAPIKeys = model.loadAPIKeys()
            devices = try await loadedDevices
            apiKeys = try await loadedAPIKeys
            errorMessage = ""
        } catch {
            setErrorMessage(for: error)
        }
    }

    private func performAction(action: @escaping () async throws -> Void) async {
        guard !isWorking else {
            return
        }
        isWorking = true
        defer { isWorking = false }
        do {
            try await action()
            errorMessage = ""
            if model.isAuthenticated, !model.emailVerificationRequired {
                devices = try await model.loadAccountDevices()
                apiKeys = try await model.loadAPIKeys()
            }
        } catch {
            setErrorMessage(for: error)
        }
    }

    private func reloadDevices() async {
        guard !isWorking else {
            return
        }
        guard model.isAuthenticated, !model.emailVerificationRequired else {
            return
        }
        do {
            devices = try await model.loadAccountDevices()
            errorMessage = ""
        } catch {
            setErrorMessage(for: error)
        }
    }

    private func reloadAPIKeys() async {
        guard !isWorking else {
            return
        }
        guard model.isAuthenticated, !model.emailVerificationRequired else {
            return
        }
        do {
            apiKeys = try await model.loadAPIKeys()
            errorMessage = ""
        } catch {
            setErrorMessage(for: error)
        }
    }

    private func setErrorMessage(for error: Error) {
        if error is CancellationError {
            return
        }
        if let urlError = error as? URLError, urlError.code == .cancelled {
            return
        }
        errorMessage = error.localizedDescription
    }

    private var otherDevices: [AccountDevice] {
        devices.filter { $0.device_id != model.deviceID }
    }
}

struct AddSoundsView: View {
    @ObservedObject var model: MGNotifyModel
    let didFinishAdding: () -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var pendingSounds: [PendingSoundImport] = []
    @State private var isAdding = false

    private var importDescription: String {
        if pendingSounds.count == 1 {
            return "You are about to add the following sound to MG Notify:"
        }
        return "You are about to add the following \(pendingSounds.count) sounds to MG Notify:"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Add Sounds")
                .font(.largeTitle.bold())
                .frame(maxWidth: .infinity, alignment: .leading)

            Text(importDescription)
                .foregroundStyle(.secondary)

            List(pendingSounds) { sound in
                Text(sound.displayName)
            }
            .listStyle(.plain)

            Button {
                addSounds()
            } label: {
                HStack {
                    Text("Add")
                    if isAdding {
                        ProgressView()
                    }
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .disabled(pendingSounds.isEmpty || isAdding)

            Button("Cancel") {
                model.clearPendingSoundImports()
                dismiss()
            }
            .frame(maxWidth: .infinity)
            .buttonStyle(.bordered)
        }
        .padding()
        .navigationTitle("Add Sounds")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            pendingSounds = model.pendingSoundImports()
        }
    }

    private func addSounds() {
        guard !pendingSounds.isEmpty else {
            return
        }

        isAdding = true
        Task {
            let shouldSelectImportedSound = pendingSounds.count == 1
            await model.addPendingSoundImports(selectSingleImportedSound: shouldSelectImportedSound)
            pendingSounds = []
            isAdding = false
            didFinishAdding()
        }
    }
}

struct SoundsView: View {
    @Environment(\.isSearching) private var isSearching
    @ObservedObject var model: MGNotifyModel
    @FocusState private var isSoundSearchFocused: Bool
    @State private var isImporting = false
    @State private var didPerformInitialScroll = false
    @State private var showingDeleteAllSoundsConfirmation = false
    @State private var soundSearchText = ""

    /// Anything iOS can read; it's converted if need be (see SoundLibrary).
    private static let supportedSoundTypes: [UTType] = [.audio]

    private var options: [SoundOption] {
        model.soundOptions()
    }

    private var filteredOptions: [SoundOption] {
        let searchableOptions = isSoundSearchActive
            ? options.filter { $0.id != "__none__" && $0.id != "default" }
            : options
        let query = soundSearchText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else {
            return searchableOptions
        }
        return searchableOptions.filter { option in
            option.title.localizedCaseInsensitiveContains(query)
        }
    }

    private var isSoundSearchActive: Bool {
        isSearching || isSoundSearchFocused || !soundSearchText.isEmpty
    }

    var body: some View {
        ScrollViewReader { proxy in
            List {
                if !isSoundSearchActive {
                    Section {
                        Text("Imported sounds must be 30 seconds or less. Any sound longer than 30 seconds will be ignored.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .padding(.vertical, 4)
                        if !model.importedSounds.isEmpty {
                            Text("You currently have \(model.importedSounds.count) added sound\(model.importedSounds.count == 1 ? "" : "s").")
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        }
                    }
                }

                if filteredOptions.isEmpty {
                    Section {
                        Text("No matching sounds.")
                            .foregroundStyle(.secondary)
                    }
                } else {
                    ForEach(Array(filteredOptions.enumerated()), id: \.element.id) { _, option in
                        HStack {
                            Text(option.title)
                            Spacer()
                            if model.selectedSound == option.id {
                                Image(systemName: "checkmark")
                                    .foregroundStyle(Color.accentColor)
                                    .accessibilityHidden(true)
                            }
                        }
                        .contentShape(Rectangle())
                        .onTapGesture {
                            model.selectedSound = option.id
                            model.previewSoundSelection()
                            Task {
                                await model.saveSelectedSound()
                            }
                        }
                        .accessibilityAddTraits(model.selectedSound == option.id ? [.isSelected] : [])
                        .id(option.id)
                        .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                            if model.importedSounds.contains(option.id) {
                                Button(role: .destructive) {
                                    model.deleteImportedSound(named: option.id)
                                } label: {
                                    Label("Delete", systemImage: "trash")
                                }
                            }
                        }
                    }
                }
            }
            .navigationTitle("Sounds")
            .navigationBarTitleDisplayMode(.inline)
            .modifier(
                SearchBarModifier(
                    isEnabled: !model.importedSounds.isEmpty,
                    searchText: $soundSearchText,
                    isFocused: $isSoundSearchFocused,
                    prompt: "Search sounds"
                )
            )
            .toolbar {
                if !model.importedSounds.isEmpty {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button(role: .destructive) {
                            showingDeleteAllSoundsConfirmation = true
                        } label: {
                            Image(systemName: "trash")
                        }
                        .accessibilityLabel("Delete All")
                    }
                }

                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        isImporting = true
                    } label: {
                        Image(systemName: "plus")
                    }
                    .accessibilityLabel("Add")
                }
            }
            .task {
                guard !didPerformInitialScroll else {
                    return
                }
                didPerformInitialScroll = true
                guard options.contains(where: { $0.id == model.selectedSound }) else {
                    return
                }
                await MainActor.run {
                    proxy.scrollTo(model.selectedSound, anchor: .center)
                }
            }
        }
        .confirmationDialog(
            "Are you sure you wish to delete all sounds?",
            isPresented: $showingDeleteAllSoundsConfirmation,
            titleVisibility: Visibility.visible
        ) {
            Button("Delete All", role: .destructive) {
                model.deleteAllImportedSounds()
            }
            Button("Cancel", role: .cancel) {}
        }
        .fileImporter(
            isPresented: $isImporting,
            allowedContentTypes: Self.supportedSoundTypes,
            allowsMultipleSelection: true
        ) { result in
            if case let .success(urls) = result {
                let scopedURLs = urls.map { url in
                    (url, url.startAccessingSecurityScopedResource())
                }
                Task {
                    defer {
                        for (url, didStartAccess) in scopedURLs where didStartAccess {
                            url.stopAccessingSecurityScopedResource()
                        }
                    }
                    let didImportAtLeastOne = await model.importSounds(from: scopedURLs.map(\.0))
                    if didImportAtLeastOne {
                        await model.saveSelectedSound()
                    }
                }
            }
        }
        .onAppear {
            model.reloadImportedSounds()
        }
        .onChange(of: model.importedSounds.count) { _, count in
            if count == 0 {
                soundSearchText = ""
            }
        }
    }
}

private struct SearchBarModifier: ViewModifier {
    let isEnabled: Bool
    @Binding var searchText: String
    @FocusState.Binding var isFocused: Bool
    let prompt: String

    func body(content: Content) -> some View {
        if isEnabled {
            content
                .searchable(
                    text: $searchText,
                    placement: .automatic,
                    prompt: prompt
                )
                .searchFocused($isFocused)
        } else {
            content
        }
    }
}

#Preview {
    ContentView()
}
