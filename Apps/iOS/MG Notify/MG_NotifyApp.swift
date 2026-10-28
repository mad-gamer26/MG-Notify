import SwiftUI
import UserNotifications
import UIKit

final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        let token = deviceToken.map { String(format: "%02x", $0) }.joined()
        Task { @MainActor in
            MGNotifyModel.shared.updateDeviceToken(token)
        }
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        Task { @MainActor in
            MGNotifyModel.shared.updateRegistrationError(error)
        }
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        let userInfo = notification.request.content.userInfo
        // The app is in front: speak the notification and play its sound
        // instead of showing a banner.
        await MGNotifyModel.shared.announcePush(userInfo: userInfo)
        _ = await MGNotifyModel.shared.receiveNotification(userInfo: userInfo)
        // Nothing is shown, but the app badge still follows the server's count.
        return [.badge]
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        _ = await MGNotifyModel.shared.receiveNotification(
            userInfo: response.notification.request.content.userInfo,
            openedByUser: true
        )
    }

    func application(
        _ application: UIApplication,
        didReceiveRemoteNotification userInfo: [AnyHashable: Any],
        fetchCompletionHandler completionHandler: @escaping (UIBackgroundFetchResult) -> Void
    ) {
        Task {
            let result = await MGNotifyModel.shared.receiveNotification(userInfo: userInfo)
            switch result {
            case .ignored:
                completionHandler(.noData)
            case .refreshed, .remoteEvent:
                completionHandler(.newData)
            }
        }
    }
}

@main
struct MG_NotifyApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}
