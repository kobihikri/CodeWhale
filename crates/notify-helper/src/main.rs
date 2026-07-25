//! `codewhale-notify` — post a macOS notification carrying the Codewhale icon.
//!
//! # Why this exists
//!
//! Notification Center attributes every alert to the **bundle that posted it**,
//! and shows that bundle's icon. There is no per-notification icon parameter:
//! AppleScript's `display notification` has none, and `NSUserNotification`'s
//! `contentImage` was a right-hand attachment rather than the app icon (and is
//! deprecated regardless). So "give the notification our icon" can only ever be
//! solved as "post it from a bundle whose icon is ours."
//!
//! The TUI is a bare CLI with no bundle identifier, so it cannot post at all
//! through `UNUserNotificationCenter` — `currentNotificationCenter` raises when
//! the process has no bundle. Shelling out to `osascript` works but is
//! attributed to Script Editor, which is the generic icon users were seeing.
//!
//! This binary is the executable inside a small `Codewhale Notify.app` that the
//! TUI assembles on first use (Info.plist + `codewhale.icns` + this binary).
//! Running inside that bundle gives the process a bundle identifier and the
//! Codewhale icon, and the alert is attributed accordingly.
//!
//! Usage: `codewhale-notify <title> <body> [subtitle]`
//!
//! Exit codes: `0` posted, `1` bad arguments, `2` the platform refused
//! (no bundle identity, authorization denied, or the post errored). The caller
//! is expected to treat a non-zero exit as "fall back to the osascript path"
//! rather than as a hard failure — a missing notification must never take down
//! a turn.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("codewhale-notify is macOS-only");
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: codewhale-notify <title> <body> [subtitle]");
        std::process::exit(1);
    }
    let title = args[0].clone();
    let body = args[1].clone();
    let subtitle = args.get(2).cloned();

    match macos::post(&title, &body, subtitle.as_deref()) {
        Ok(()) => std::process::exit(0),
        Err(err) => {
            eprintln!("codewhale-notify: {err}");
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSBundle, NSDate, NSError, NSRunLoop, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
        UNNotificationSound, UNUserNotificationCenter,
    };

    /// How long to let the async authorization + delivery callbacks land before
    /// giving up. A notification is best-effort garnish on a turn; it must never
    /// hold the caller open. The TUI already spawns this off its own thread, so
    /// this ceiling only bounds a stuck helper process.
    const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5);

    pub fn post(title: &str, body: &str, subtitle: Option<&str>) -> Result<(), String> {
        // `currentNotificationCenter` does not return an error when the process
        // has no bundle identifier — it raises
        // `NSInternalInconsistencyException` ("bundleProxyForCurrentProcess is
        // nil") and aborts the process with SIGABRT. Rust cannot catch that, so
        // the check has to happen BEFORE the call. Verified by running this
        // binary outside its bundle: exit 134 with an ObjC stack trace.
        //
        // This is the whole reason the binary ships inside
        // `Codewhale Notify.app`; running it bare is a programming error, and
        // the caller needs a clean non-zero exit so it can fall back to
        // osascript instead of inheriting a crash.
        // Authorization is only offered to a real application. Without an
        // NSApplication instance the request silently resolves to
        // "Notifications are not allowed for this application" — observed
        // directly, from both ~/Applications and Application Support, so the
        // denial is about app identity rather than install location.
        let mtm = objc2_foundation::MainThreadMarker::new()
            .ok_or_else(|| "must run on the main thread".to_string())?;
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();

        let has_bundle_id = NSBundle::mainBundle()
            .bundleIdentifier()
            .is_some_and(|id| !id.to_string().is_empty());
        if !has_bundle_id {
            return Err(
                "no bundle identifier: run this from inside Codewhale Notify.app".to_string(),
            );
        }

        let center = unsafe { UNUserNotificationCenter::currentNotificationCenter() };

        request_authorization(&center)?;

        let content = unsafe { UNMutableNotificationContent::new() };
        unsafe {
            content.setTitle(&NSString::from_str(title));
            content.setBody(&NSString::from_str(body));
            if let Some(subtitle) = subtitle {
                content.setSubtitle(&NSString::from_str(subtitle));
            }
            content.setSound(Some(&UNNotificationSound::defaultSound()));
        }

        // A fresh identifier per post: reusing one replaces the previous alert
        // in place, which would silently swallow a second approval prompt while
        // the first is still on screen.
        let identifier = NSString::from_str(&format!(
            "net.codewhale.notify.{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));

        let request = unsafe {
            UNNotificationRequest::requestWithIdentifier_content_trigger(
                &identifier,
                &content,
                None, // nil trigger => deliver immediately
            )
        };

        let (tx, rx) = mpsc::channel::<Option<String>>();
        let completion = RcBlock::new(move |error: *mut NSError| {
            let message = if error.is_null() {
                None
            } else {
                Some(unsafe { (*error).localizedDescription() }.to_string())
            };
            let _ = tx.send(message);
        });

        unsafe {
            center.addNotificationRequest_withCompletionHandler(&request, Some(&completion));
        }

        match pump_until(&rx) {
            Some(None) => Ok(()),
            Some(Some(err)) => Err(format!("delivery failed: {err}")),
            None => Err("timed out waiting for delivery".to_string()),
        }
    }

    fn request_authorization(center: &UNUserNotificationCenter) -> Result<(), String> {
        let (tx, rx) = mpsc::channel::<Result<(), String>>();
        let completion = RcBlock::new(move |granted: Bool, error: *mut NSError| {
            let outcome = if !error.is_null() {
                Err(unsafe { (*error).localizedDescription() }.to_string())
            } else if granted.as_bool() {
                Ok(())
            } else {
                Err("authorization denied".to_string())
            };
            let _ = tx.send(outcome);
        });

        unsafe {
            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &completion,
            );
        }

        match pump_until(&rx) {
            Some(result) => result,
            None => Err("timed out waiting for authorization".to_string()),
        }
    }

    /// Spin the main run loop while waiting for an ObjC completion block.
    ///
    /// These callbacks are dispatched to the main queue, so a plain blocking
    /// `recv` would deadlock: nothing would ever drain the queue that delivers
    /// the answer we are blocking on.
    fn pump_until<T>(rx: &mpsc::Receiver<T>) -> Option<T> {
        let deadline = std::time::Instant::now() + CALLBACK_TIMEOUT;
        loop {
            if let Ok(value) = rx.try_recv() {
                return Some(value);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            unsafe {
                let until = NSDate::dateWithTimeIntervalSinceNow(0.05);
                NSRunLoop::mainRunLoop().runUntilDate(&until);
            }
        }
    }
}
