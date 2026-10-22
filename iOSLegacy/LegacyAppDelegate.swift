import UIKit

// OpenDisplay for iOS 12: the receiver for the A7/A8 devices the main app's
// iOS 15 floor leaves out (iPad Air, iPad mini 2/3, iPhone 5s/6). UIKit
// instead of SwiftUI; the receiver, the lifecycle policy and the video/touch
// view are the main app's own files.
//
// Two lifecycles: iOS 12 has no scenes, so the app delegate owns the window.
// Apps built with a current SDK must adopt scenes wherever they exist, so on
// iOS 13+ (development and simulator runs) LegacySceneDelegate owns it and
// forwards foreground/background here. Info.plist declares the scene; iOS 12
// ignores that key.

extension Notification.Name {
    static let deviceDidShake = Notification.Name("deviceDidShake")
}

/// Shake opens settings, as in the main app.
final class ShakeWindow: UIWindow {
    override func motionEnded(_ motion: UIEvent.EventSubtype, with event: UIEvent?) {
        if motion == .motionShake {
            NotificationCenter.default.post(name: .deviceDidShake, object: nil)
        }
        super.motionEnded(motion, with: event)
    }
}

@UIApplicationMain
final class LegacyAppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?
    private let model = ReceiverModel()
    // The first activation is the launch itself: the listener is still
    // coming up and must not be "repaired" by ensureListening().
    private var hasBeenActive = false

    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        Log.info("legacy receiver launched on iOS \(UIDevice.current.systemVersion) (\(DecodeBudget.currentModel))")
        if #available(iOS 13, *) {
            // LegacySceneDelegate builds the window.
        } else {
            let window = ShakeWindow(frame: UIScreen.main.bounds)
            install(in: window)
            self.window = window
        }
        application.isIdleTimerDisabled = true
        model.start()
        return true
    }

    func install(in window: UIWindow) {
        window.rootViewController = ReceiverViewController(model: model)
        window.makeKeyAndVisible()
    }

    // MARK: - Foreground / background (from the app on iOS 12, the scene after)

    func didBecomeActive() {
        guard hasBeenActive else {
            hasBeenActive = true
            return
        }
        Log.info("app became active")
        model.sceneDidActivate()
    }

    func didEnterBackground() {
        Log.info("app entered background")
        model.sceneDidBackground()
    }

    func applicationDidBecomeActive(_ application: UIApplication) { didBecomeActive() }
    func applicationDidEnterBackground(_ application: UIApplication) { didEnterBackground() }

    // The deliberate "screen off" signal: locking the device makes protected
    // data unavailable (a plain app switch doesn't). See ReceiverModel.
    func applicationProtectedDataWillBecomeUnavailable(_ application: UIApplication) {
        Log.info("protected data will become unavailable (device locking)")
        model.deviceWillLock()
    }

    func applicationProtectedDataDidBecomeAvailable(_ application: UIApplication) {
        Log.info("protected data available again (device unlocked)")
        model.deviceDidUnlock()
    }

    func applicationWillTerminate(_ application: UIApplication) {
        model.appWillTerminate()
    }
}

@available(iOS 13.0, *)
@objc(LegacySceneDelegate)
final class LegacySceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    private var app: LegacyAppDelegate? { UIApplication.shared.delegate as? LegacyAppDelegate }

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession,
               options connectionOptions: UIScene.ConnectionOptions) {
        guard let windowScene = scene as? UIWindowScene else { return }
        let window = ShakeWindow(windowScene: windowScene)
        app?.install(in: window)
        self.window = window
    }

    func sceneDidBecomeActive(_ scene: UIScene) { app?.didBecomeActive() }
    func sceneDidEnterBackground(_ scene: UIScene) { app?.didEnterBackground() }
}
