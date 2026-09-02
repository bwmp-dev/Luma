use crate::errors::{LumaError, Result};

/*
 * In-app browser bridge, using an @_cdecl entry point in
 * gen/apple/Sources/luma/LumaBrowser.swift, reached directly from the static
 * library — the same route as the native menu bridge in menu.rs.
 *
 * This exists because of who serves the URL. A web preview is a loopback port
 * forwarded by a tunnel inside THIS process, so sending the system browser to
 * it backgrounds Luma, and iOS suspends a backgrounded app within seconds: the
 * tunnel stops answering and the page hangs half-loaded. A browser the app
 * presents keeps the app foreground-active, so the tunnel keeps running for
 * exactly as long as the page is being read.
 */

/// Open `url` in a browser hosted inside the app.
///
/// Registered on every platform so the frontend has one call to make, and it
/// simply errors where there is no such browser (Android, desktop) or where the
/// URL is not one it can show. Either error is the frontend's signal to fall
/// back to the system handler, and nothing is lost by that off iOS: no other
/// platform suspends the app that is serving the tunnel.
#[tauri::command]
pub fn browser_open_in_app(url: String) -> Result<()> {
    // Only ever a web page. The in-app browser cannot render anything else, and
    // keeping the check here means the frontend cannot reach a system handler
    // for some other scheme through a command meant for previews.
    let scheme = url
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !url.contains(':') || (scheme != "http" && scheme != "https") {
        return Err(LumaError::InvalidInput(
            "the in-app browser only opens http and https URLs".into(),
        ));
    }
    imp::open(&url)
}

#[cfg(target_os = "ios")]
mod imp {
    use crate::errors::{LumaError, Result};
    use std::ffi::CString;
    use std::os::raw::c_char;

    extern "C" {
        fn luma_browser_present(url: *const c_char) -> bool;
    }

    pub fn open(url: &str) -> Result<()> {
        let url = CString::new(url)
            .map_err(|_| LumaError::InvalidInput("url contains a null byte".into()))?;
        // SAFETY: Swift copies the string before returning and never retains the
        // pointer, so the CString may be dropped after the call.
        let presented = unsafe { luma_browser_present(url.as_ptr()) };
        if !presented {
            return Err(LumaError::InvalidInput(
                "no in-app browser could be presented".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(not(target_os = "ios"))]
mod imp {
    use crate::errors::{LumaError, Result};

    pub fn open(_url: &str) -> Result<()> {
        Err(LumaError::InvalidInput(
            "the in-app browser is only available on iOS".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::browser_open_in_app;

    /// Every scheme is rejected before it can reach a platform handler. On iOS
    /// this is belt-and-braces (SFSafariViewController would refuse too, after
    /// trapping on the initializer); off iOS it is the only check there is.
    #[test]
    fn rejects_everything_that_is_not_a_web_page() {
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ssh://host",
            "127.0.0.1:8080",
            "",
        ] {
            let error = browser_open_in_app(url.to_string()).unwrap_err();
            assert!(
                error.to_string().contains("http and https"),
                "{url} was not rejected as a non-web URL: {error}"
            );
        }
    }

    /// A web URL gets past the scheme check and on to the platform, which off
    /// iOS declines — the frontend's cue to use the system browser instead.
    #[test]
    #[cfg(not(target_os = "ios"))]
    fn passes_web_urls_through_to_the_platform() {
        let error = browser_open_in_app("http://127.0.0.1:49152/".to_string()).unwrap_err();
        assert!(
            error.to_string().contains("only available on iOS"),
            "{error}"
        );
    }
}
