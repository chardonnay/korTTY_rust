use std::cell::OnceCell;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;

use block2::{Block, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{ClassType, DeclaredClass, declare_class, msg_send_id, mutability};
use objc2_foundation::{
    MainThreadMarker, NSBundle, NSError, NSObject, NSObjectProtocol, NSString, NSUUID,
};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

type Activate = Box<dyn FnOnce() + Send>;
const IDENTIFIER_PREFIX: &str = "kortty.coding-agent.";
const MAX_ACTIVATIONS: usize = 64;
static ACTIVATIONS: Mutex<VecDeque<(String, Activate)>> = Mutex::new(VecDeque::new());

thread_local! {
    // UNUserNotificationCenter holds the delegate weakly.
    static DELEGATE: OnceCell<Retained<NotificationDelegate>> = const { OnceCell::new() };
}

declare_class!(
    struct NotificationDelegate;

    // SAFETY: NSObject allows subclassing. This delegate stores no instance data.
    unsafe impl ClassType for NotificationDelegate {
        type Super = NSObject;
        type Mutability = mutability::InteriorMutable;
        const NAME: &'static str = "KorttyDesktopNotificationDelegate";
    }

    impl DeclaredClass for NotificationDelegate {}
    unsafe impl NSObjectProtocol for NotificationDelegate {}

    // SAFETY: Both methods match the optional UserNotifications protocol methods.
    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        #[method(userNotificationCenter:willPresentNotification:withCompletionHandler:)]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            notification: &UNNotification,
            completion: &Block<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            // SAFETY: The framework supplies a live notification and request.
            let id = unsafe { notification.request().identifier() };
            let options = if id.to_string().starts_with(IDENTIFIER_PREFIX) {
                UNNotificationPresentationOptions::UNNotificationPresentationOptionBanner
            } else {
                UNNotificationPresentationOptions::empty()
            };
            completion.call((options,));
        }

        #[method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:)]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &Block<dyn Fn()>,
        ) {
            // SAFETY: The framework supplies a live response and its notification objects.
            let (id, is_default) = unsafe {
                let request = response.notification().request();
                (
                    request.identifier().to_string(),
                    response.actionIdentifier().as_ref() == UNNotificationDefaultActionIdentifier,
                )
            };
            let activate = take_activation(&id);
            completion.call(());
            if is_default && let Some(activate) = activate {
                let _ = catch_unwind(AssertUnwindSafe(activate));
            }
        }
    }
);

/// Installs the notification delegate on the main thread before the app finishes launching.
pub fn initialize_desktop_notifications() {
    if MainThreadMarker::new().is_none() || DELEGATE.with(|slot| slot.get().is_some()) {
        return;
    }
    let bundle = NSBundle::mainBundle();
    // SAFETY: These read-only queries borrow a live main bundle. UserNotifications
    // requires a packaged app identity.
    if !unsafe { bundle.bundlePath() }.to_string().ends_with(".app")
        || unsafe { bundle.bundleIdentifier() }.is_none()
    {
        return;
    }
    // SAFETY: The packaged app can use the shared notification center. Leave any
    // delegate supplied by another component in place.
    let center = unsafe { UNUserNotificationCenter::currentNotificationCenter() };
    if unsafe { center.delegate() }.is_some() {
        return;
    }
    DELEGATE.with(|slot| {
        // SAFETY: NSObject's initializer initializes the superclass of this unit-ivar class.
        let delegate = slot.get_or_init(|| unsafe {
            msg_send_id![super(NotificationDelegate::alloc().set_ivars(())), init]
        });
        // SAFETY: The delegate conforms to the protocol and remains strongly retained.
        unsafe { center.setDelegate(Some(ProtocolObject::from_ref(&**delegate))) };
    });
}

fn active_notification_center() -> Option<Retained<UNUserNotificationCenter>> {
    MainThreadMarker::new()?;
    DELEGATE.with(|slot| {
        let expected = ProtocolObject::from_ref(&**slot.get()?);
        // SAFETY: A retained delegate is installed only for a packaged app identity.
        // Both queries run on the main thread.
        let center = unsafe { UNUserNotificationCenter::currentNotificationCenter() };
        let current = unsafe { center.delegate() }?;
        std::ptr::eq(&*current, expected).then_some(center)
    })
}

/// Starts an alert submission on the main thread, subject to asynchronous system authorization.
/// Returns false if the app's notification delegate or permission gate is unavailable.
/// The activation callback runs at most once on the notification center's queue.
pub fn send_desktop_notification(
    title: String,
    body: String,
    still_allowed: impl Fn() -> bool + Send + Sync + 'static,
    on_activate: impl FnOnce() + Send + 'static,
) -> bool {
    let Some(center) = active_notification_center().filter(|_| still_allowed()) else {
        return false;
    };
    let id = format!("{IDENTIFIER_PREFIX}{}", NSUUID::UUID().UUIDString());
    let evicted = {
        let Ok(mut activations) = ACTIVATIONS.lock() else {
            return false;
        };
        activations.push_back((id.clone(), Box::new(on_activate)));
        if activations.len() > MAX_ACTIVATIONS {
            activations.pop_front()
        } else {
            None
        }
    };
    drop(evicted);

    let authorized = RcBlock::new(move |granted: Bool, error: *mut NSError| {
        if granted.as_bool()
            && error.is_null()
            && catch_unwind(AssertUnwindSafe(&still_allowed)).unwrap_or(false)
        {
            submit_alert(&id, &title, &body);
        } else {
            drop(take_activation(&id));
        }
    });
    // SAFETY: The center copies the block for later use. Its captures are safe to
    // share across threads, and activation state is protected by its mutex.
    unsafe {
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::UNAuthorizationOptionAlert,
            &authorized,
        );
    }
    true
}

fn submit_alert(id: &str, title: &str, body: &str) {
    let id_for_failure = id.to_owned();
    let submitted = RcBlock::new(move |error: *mut NSError| {
        if !error.is_null() {
            drop(take_activation(&id_for_failure));
        }
    });
    // SAFETY: UserNotifications permits submitting requests from any thread. The
    // initialized content and request stay retained through submission. The center
    // copies the completion block, which captures only the owned request ID.
    unsafe {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(id),
            &content,
            None,
        );
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, Some(&submitted));
    }
}

fn take_activation(id: &str) -> Option<Activate> {
    let mut activations = ACTIVATIONS.lock().ok()?;
    let index = activations.iter().position(|(stored, _)| stored == id)?;
    activations.remove(index).map(|(_, activate)| activate)
}
