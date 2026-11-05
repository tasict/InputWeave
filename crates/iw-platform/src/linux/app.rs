//! Opening the app on Linux: through
//! D-Bus activation of the app's own name, so the session's D-Bus (and
//! with it systemd, by the service file's `SystemdService=`) starts it in a
//! unit of its own, where the portals know it by its app id, not in the
//! agent's. `background` is the app's action that only makes sure it runs,
//! with its tray icon, and opens nothing: what the user has to see opens by
//! itself once it connects, as the pairing code does.

/// The app's object, as GApplication makes it of its id, which is also
/// its bus name ([`APP_ID`](super::wayland::APP_ID)).
pub(crate) const APP_PATH: &str = "/cc/ctsite/inputweave";

/// Asks D-Bus to start the app if it is not running, on a thread of its
/// own; false if that thread cannot start. How long the app takes to
/// start, and whether it does, is only logged.
#[cfg(target_os = "linux")]
pub(crate) fn activate() -> bool {
    use std::time::Duration;

    /// Starting the app may take a while on a busy login.
    const WAIT: Duration = Duration::from_secs(25);

    let started = std::thread::Builder::new()
        .name("inputweave-open-app".to_owned())
        .spawn(|| {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot open the app");
                    return;
                }
            };
            let result = runtime.block_on(tokio::time::timeout(WAIT, async {
                let conn = super::bus::session(None).await?;
                let empty: Vec<zbus::zvariant::Value<'_>> = Vec::new();
                let data: std::collections::HashMap<&str, zbus::zvariant::Value<'_>> =
                    std::collections::HashMap::new();
                conn.call_method(
                    Some(super::wayland::APP_ID),
                    APP_PATH,
                    Some("org.freedesktop.Application"),
                    "ActivateAction",
                    &("background", empty, data),
                )
                .await
                .map(|_| ())
            }));
            match result {
                Ok(Ok(())) => tracing::debug!("the app was asked to run"),
                Ok(Err(e)) => tracing::warn!(error = %e, "cannot open the app over D-Bus"),
                Err(_) => tracing::warn!("the app did not answer over D-Bus in time"),
            }
        });
    started.is_ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_apps_object_is_made_of_its_id() {
        let id = super::super::wayland::APP_ID;
        assert_eq!(super::APP_PATH, format!("/{}", id.replace('.', "/")));
    }
}
