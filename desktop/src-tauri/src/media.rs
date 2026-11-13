//! Microphone and camera for calls (ADR 0028), on Windows (WebView2).
//!
//! The call screen is an ordinary web page, so the webview asks for the microphone on its behalf. Left to
//! its default, WebView2 shows its own prompt, and on some machines (a policy, an earlier refusal, a page
//! with no visible prompt) the answer is "no" and the call has no sound. This answers the request itself:
//! microphone and camera only, for this app's own page only, and only while a call is up (`set_active`).
//! Every other permission keeps WebView2's default.
//!
//! Two more things help on Windows. Chromium hides a computer's real addresses behind `.local` names
//! (mDNS) in call setup; some networks do not resolve those, so two computers on the same Wi-Fi cannot
//! reach each other. `WebRtcHideLocalIpsWithMdns` is switched off here (see `configure_webview`). And
//! the app's page is served from `http://tauri.localhost`, which is what the check below looks for.

use std::sync::atomic::{AtomicBool, Ordering};

static CALL_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether a call screen is up. Only then is the microphone and camera granted to the page.
pub fn set_active(on: bool) {
    CALL_ACTIVE.store(on, Ordering::SeqCst);
}

/// Must run before the first webview is created: WebView2 reads its extra switches then.
pub fn configure_webview() {
    const SWITCH: &str = "--disable-features=WebRtcHideLocalIpsWithMdns";
    let mut args = std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").unwrap_or_default();
    if args.contains(SWITCH) {
        return;
    }
    // Keep whatever is already there (a debugging port, for example). Chromium takes one
    // --disable-features switch, so a second one is merged into the first.
    if let Some(at) = args.find("--disable-features=") {
        let value_start = at + "--disable-features=".len();
        args.insert_str(value_start, "WebRtcHideLocalIpsWithMdns,");
    } else {
        if !args.is_empty() {
            args.push(' ');
        }
        args.push_str(SWITCH);
    }
    std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", args);
}

/// Answer the microphone and camera requests of this app's page while a call is up.
#[cfg(windows)]
pub fn install(window: &tauri::WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_CAMERA,
        COREWEBVIEW2_PERMISSION_KIND_MICROPHONE, COREWEBVIEW2_PERMISSION_STATE_ALLOW,
    };
    use webview2_com::{take_pwstr, PermissionRequestedEventHandler};

    let _ = window.with_webview(|wv| unsafe {
        let Ok(core) = wv.controller().CoreWebView2() else {
            return;
        };
        let handler = PermissionRequestedEventHandler::create(Box::new(|_sender, args| {
            let Some(args) = args else { return Ok(()) };
            let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
            args.PermissionKind(&mut kind)?;
            if kind != COREWEBVIEW2_PERMISSION_KIND_MICROPHONE
                && kind != COREWEBVIEW2_PERMISSION_KIND_CAMERA
            {
                return Ok(());
            }
            let mut uri = Default::default();
            args.Uri(&mut uri)?;
            let uri = take_pwstr(uri);
            let ours = uri.starts_with("http://tauri.localhost/")
                || uri.starts_with("https://tauri.localhost/");
            if ours && CALL_ACTIVE.load(Ordering::SeqCst) {
                args.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW)?;
            }
            Ok(())
        }));
        let mut token = 0i64;
        let _ = core.add_PermissionRequested(&handler, &mut token);
    });
}

#[cfg(not(windows))]
pub fn install(_window: &tauri::WebviewWindow) {}
