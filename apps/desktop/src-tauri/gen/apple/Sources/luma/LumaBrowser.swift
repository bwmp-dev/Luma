import Foundation
import SafariServices
import UIKit

/*
 * In-app browser bridge.
 *
 * A web-preview URL points at a loopback port served by a tunnel running inside
 * THIS process, so handing it to Safari is self-defeating: iOS backgrounds Luma,
 * suspends it seconds later, and the tunnel stops answering — the page the user
 * just asked for hangs half-loaded, and the app looks broken rather than
 * backgrounded.
 *
 * SFSafariViewController is presented BY the app instead of in place of it, so
 * Luma stays foreground-active and keeps serving the tunnel for as long as the
 * page is being read. It is also a real Safari — same engine, same reader, same
 * certificate handling — rather than a WKWebView we would have to grow an
 * address bar, history and a share sheet onto, and it carries its own "Open in
 * Safari" action for the times leaving really is what the user wants.
 */

/// Present `url` in an in-app browser. Returns false when it could not be shown
/// — no foreground window, or a scheme SFSafariViewController refuses — which
/// Rust reports as an error so the frontend falls back to the system handler.
@_cdecl("luma_browser_present")
func lumaBrowserPresent(_ url: UnsafePointer<CChar>?) -> Bool {
  guard let url, let target = URL(string: String(cString: url)) else { return false }
  // SFSafariViewController's initializer traps on any other scheme, so this is
  // a guard against a crash, not a policy check (Rust already enforces one).
  let scheme = target.scheme?.lowercased()
  guard scheme == "http" || scheme == "https" else { return false }
  if Thread.isMainThread {
    return LumaBrowserController.shared.present(target)
  }
  return DispatchQueue.main.sync {
    LumaBrowserController.shared.present(target)
  }
}

private final class LumaBrowserController: NSObject, SFSafariViewControllerDelegate {
  static let shared = LumaBrowserController()

  /// The browser currently on screen, so reopening a preview replaces it rather
  /// than stacking a second sheet the user has to dismiss twice.
  private weak var presented: SFSafariViewController?

  func present(_ url: URL) -> Bool {
    guard let host = LumaBrowserController.topViewController() else { return false }

    let browser = SFSafariViewController(url: url)
    browser.delegate = self
    browser.dismissButtonStyle = .close
    // The preview is a page the user is stepping into and back out of, not a
    // link they are following away from the app, so it presents as a sheet they
    // can swipe down rather than as a full takeover.
    browser.modalPresentationStyle = .pageSheet

    if let existing = presented, existing.presentingViewController != nil {
      // Dismiss before presenting: UIKit ignores a presentation requested while
      // another is still on screen, which would silently do nothing.
      existing.dismiss(animated: false) { [weak self] in
        self?.show(browser, from: host)
      }
    } else {
      show(browser, from: host)
    }
    presented = browser
    return true
  }

  private func show(_ browser: SFSafariViewController, from host: UIViewController) {
    // The host may have presented something else in the meantime (or been
    // dismissed itself), so resolve the top controller again at the last moment.
    let target = LumaBrowserController.topViewController() ?? host
    target.present(browser, animated: true)
  }

  func safariViewControllerDidFinish(_ controller: SFSafariViewController) {
    if presented === controller { presented = nil }
  }

  /// Deepest presented controller of the foreground window, which is what a new
  /// sheet has to be presented from — presenting from a controller that is
  /// already covered is a UIKit no-op.
  private static func topViewController() -> UIViewController? {
    let scenes = UIApplication.shared.connectedScenes
      .compactMap { $0 as? UIWindowScene }
      .filter { $0.activationState == .foregroundActive }
    let window =
      scenes.flatMap({ $0.windows }).first(where: { $0.isKeyWindow })
      ?? scenes.flatMap({ $0.windows }).first
    var controller = window?.rootViewController
    while let next = controller?.presentedViewController {
      controller = next
    }
    return controller
  }
}
