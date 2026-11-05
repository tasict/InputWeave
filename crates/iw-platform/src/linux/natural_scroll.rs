//! Natural scrolling on a Wayland desktop, which turns the direction of
//! scrolling over before capture sees it: the InputCapture portal hands
//! over scrolling as the desktop applies it (libinput's natural scrolling
//! comes first), so it is turned back here, as the X11 backend does from
//! libinput's property, and scrolling reaches other devices as the wheel
//! turned.
//!
//! The desktops keep the setting per kind of device, not per event, and
//! libei does not say which device scrolled: a wheel's notches go by the
//! mouse's setting, smooth scrolling without notches by the touchpad's.
//!
//! Read from GNOME's settings (`org.gnome.desktop.peripherals.mouse` and
//! `.touchpad`, `natural-scroll`), again as dconf says they changed (its
//! `Notify` signal on the session bus, which GSettings itself follows), or
//! from KWin's input devices over D-Bus, again as `kcminputrc` (where
//! System Settings keeps them) changes; and every 30 seconds either way,
//! for what that misses (a device plugged in, a bus that went away). One
//! bus connection is kept, and made again once it fails.

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the desktop scrolls naturally, by kind of device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Natural {
    pub mouse: bool,
    pub touchpad: bool,
}

impl Natural {
    /// Whether scrolling the desktop turned over is to be turned back: a
    /// wheel's notches (`discrete`) by the mouse's setting, smooth
    /// scrolling by the touchpad's.
    pub(crate) fn turned(self, discrete: bool) -> bool {
        if discrete { self.mouse } else { self.touchpad }
    }
}

/// The desktop's natural scrolling as last read, shared with capture.
#[derive(Debug, Default)]
pub(crate) struct NaturalScroll {
    mouse: AtomicBool,
    touchpad: AtomicBool,
}

impl NaturalScroll {
    pub(crate) fn get(&self) -> Natural {
        Natural {
            mouse: self.mouse.load(Ordering::Relaxed),
            touchpad: self.touchpad.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn set(&self, natural: Natural) {
        self.mouse.store(natural.mouse, Ordering::Relaxed);
        self.touchpad.store(natural.touchpad, Ordering::Relaxed);
    }
}

/// Whether dconf's `Notify` of `changes` under `prefix` may have changed
/// the peripherals' settings: a key or folder under theirs, or a folder
/// holding theirs (a reset of everything).
pub(crate) fn touches_peripherals(prefix: &str, changes: &[String]) -> bool {
    const PERIPHERALS: &str = "/org/gnome/desktop/peripherals/";
    let empty = [String::new()];
    let changes = if changes.is_empty() {
        &empty[..]
    } else {
        changes
    };
    changes.iter().any(|change| {
        let path = format!("{prefix}{change}");
        path.starts_with(PERIPHERALS) || (path.ends_with('/') && PERIPHERALS.starts_with(&path))
    })
}

/// From KWin's input devices, each a pointer or not, a touchpad or not and
/// scrolling naturally or not: whether mice and touchpads scroll
/// naturally. Any one of a kind that does counts.
pub(crate) fn natural_of_kwin(devices: &[(bool, bool, bool)]) -> Natural {
    let mut natural = Natural::default();
    for &(pointer, touchpad, scrolls_naturally) in devices {
        if !scrolls_naturally {
            continue;
        }
        if touchpad {
            natural.touchpad = true;
        } else if pointer {
            natural.mouse = true;
        }
    }
    natural
}

#[cfg(target_os = "linux")]
mod platform {
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime};

    use futures_util::StreamExt;

    use super::{Natural, NaturalScroll, natural_of_kwin, touches_peripherals};
    use crate::linux::desktop::{Desktop, config_home, gsettings_list, gvariant_bool, listed};

    /// How often the settings are read again whatever was heard.
    const POLL: Duration = Duration::from_secs(30);
    /// How often KDE's settings file is looked at for a change.
    const LOOK: Duration = Duration::from_secs(2);
    /// How long KWin may take to answer.
    const KWIN_WAIT: Duration = Duration::from_secs(2);

    impl NaturalScroll {
        /// Reads `desktop`'s settings now and as they change, on a thread
        /// of its own, for as long as anything holds the result. Desktops
        /// whose settings are not known scroll as they come.
        pub(crate) fn watch(desktop: Desktop) -> Arc<Self> {
            let natural = Arc::new(Self::default());
            if desktop == Desktop::Other {
                return natural;
            }
            let weak = Arc::downgrade(&natural);
            let started = std::thread::Builder::new()
                .name("inputweave-natural-scroll".to_owned())
                .spawn(move || {
                    let runtime = match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => runtime,
                        Err(e) => {
                            tracing::warn!(error = %e, "cannot read natural scrolling");
                            return;
                        }
                    };
                    runtime.block_on(async move {
                        let mut settings = Settings::new(desktop);
                        let mut last = None;
                        loop {
                            let Some(natural) = weak.upgrade() else {
                                return;
                            };
                            if let Some(read) = settings.read().await {
                                if last != Some(read) {
                                    tracing::info!(
                                        mouse = read.mouse,
                                        touchpad = read.touchpad,
                                        "the desktop's natural scrolling"
                                    );
                                    last = Some(read);
                                }
                                natural.set(read);
                            }
                            drop(natural);
                            settings.changed().await;
                        }
                    });
                });
            if let Err(e) = started {
                tracing::warn!(error = %e, "cannot read natural scrolling");
            }
            natural
        }
    }

    /// The desktop's settings, and how a change in them is heard of.
    enum Settings {
        /// dconf's signals, while the bus connection lasts.
        Gnome(Option<Box<zbus::MessageStream>>),
        /// The connection to KWin, and `kcminputrc` as last looked at.
        Kde {
            conn: Option<zbus::Connection>,
            file: Option<(Option<SystemTime>, u64)>,
        },
    }

    impl Settings {
        fn new(desktop: Desktop) -> Self {
            match desktop {
                Desktop::Kde => Self::Kde {
                    conn: None,
                    file: kcminputrc(),
                },
                _ => Self::Gnome(None),
            }
        }

        async fn read(&mut self) -> Option<Natural> {
            match self {
                Self::Gnome(_) => gnome().await,
                Self::Kde { conn, .. } => kde(conn).await,
            }
        }

        /// Waits until the settings may have changed, or [`POLL`] passed.
        async fn changed(&mut self) {
            let deadline = Instant::now() + POLL;
            match self {
                Self::Gnome(signals) => {
                    if signals.is_none() {
                        *signals = dconf_signals().await;
                    }
                    let Some(stream) = signals.as_mut() else {
                        tokio::time::sleep_until(deadline.into()).await;
                        return;
                    };
                    loop {
                        let heard = tokio::time::timeout_at(deadline.into(), stream.next()).await;
                        match heard {
                            // Every POLL anyway.
                            Err(_) => return,
                            Ok(Some(Ok(message))) if peripherals_changed(&message) => return,
                            Ok(Some(_)) => {}
                            // The bus went away: connect again next time,
                            // not at once, in case it goes on doing so.
                            Ok(None) => {
                                *signals = None;
                                tokio::time::sleep_until(deadline.into()).await;
                                return;
                            }
                        }
                    }
                }
                Self::Kde { file, .. } => {
                    while Instant::now() < deadline {
                        tokio::time::sleep(LOOK).await;
                        let now = kcminputrc();
                        if now != *file {
                            *file = now;
                            return;
                        }
                    }
                }
            }
        }
    }

    /// GNOME's settings, read with one `gsettings` on a blocking thread:
    /// the peripherals' schema lists its mouse's and touchpad's.
    async fn gnome() -> Option<Natural> {
        tokio::task::spawn_blocking(|| {
            let list = gsettings_list("org.gnome.desktop.peripherals")?;
            let natural =
                |schema: &str| listed(&list, schema, "natural-scroll").and_then(gvariant_bool);
            Some(Natural {
                mouse: natural("org.gnome.desktop.peripherals.mouse")?,
                touchpad: natural("org.gnome.desktop.peripherals.touchpad")?,
            })
        })
        .await
        .ok()
        .flatten()
    }

    /// dconf's `Notify` signals on the session bus.
    async fn dconf_signals() -> Option<Box<zbus::MessageStream>> {
        let subscribe = async {
            let conn = crate::linux::bus::session(None).await?;
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("ca.desrt.dconf.Writer")?
                .member("Notify")?
                .build();
            zbus::MessageStream::for_match_rule(rule, &conn, Some(64)).await
        };
        match subscribe.await {
            Ok(stream) => Some(Box::new(stream)),
            Err(e) => {
                tracing::debug!(error = %e, "cannot hear of GNOME's settings changing");
                None
            }
        }
    }

    /// Whether a `Notify` of dconf's may be of the peripherals' settings.
    fn peripherals_changed(message: &zbus::Message) -> bool {
        message
            .body()
            .deserialize::<(String, Vec<String>, String)>()
            .is_ok_and(|(prefix, changes, _)| touches_peripherals(&prefix, &changes))
    }

    /// `kcminputrc`'s modification time and size, if it is there.
    fn kcminputrc() -> Option<(Option<SystemTime>, u64)> {
        let meta = std::fs::metadata(config_home().join("kcminputrc")).ok()?;
        Some((meta.modified().ok(), meta.len()))
    }

    /// KWin's input devices over the session bus, on `conn`, connected
    /// first if it is not; dropped if asking fails, to connect again next
    /// time.
    async fn kde(conn: &mut Option<zbus::Connection>) -> Option<Natural> {
        if conn.is_none() {
            *conn = crate::linux::bus::session(None).await.ok();
        }
        let bus = conn.as_ref()?;
        let ask = async {
            let manager = zbus::Proxy::new(
                bus,
                "org.kde.KWin",
                "/org/kde/KWin/InputDevice",
                "org.kde.KWin.InputDeviceManager",
            )
            .await?;
            let names: Vec<String> = manager.get_property("devicesSysNames").await?;
            let mut devices = Vec::new();
            for name in names {
                let path = format!("/org/kde/KWin/InputDevice/{name}");
                let Ok(device) =
                    zbus::Proxy::new(bus, "org.kde.KWin", path, "org.kde.KWin.InputDevice").await
                else {
                    continue;
                };
                let flag = |name: &'static str| {
                    let device = &device;
                    async move { device.get_property::<bool>(name).await.unwrap_or(false) }
                };
                devices.push((
                    flag("pointer").await,
                    flag("touchpad").await,
                    flag("naturalScroll").await,
                ));
            }
            zbus::Result::Ok(natural_of_kwin(&devices))
        };
        match tokio::time::timeout(KWIN_WAIT, ask).await {
            Ok(Ok(natural)) => Some(natural),
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "cannot ask KWin about natural scrolling");
                *conn = None;
                None
            }
            Err(_) => {
                *conn = None;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wheel_goes_by_the_mouse_and_smooth_scrolling_by_the_touchpad() {
        let touchpad_only = Natural {
            mouse: false,
            touchpad: true,
        };
        assert!(!touchpad_only.turned(true));
        assert!(touchpad_only.turned(false));
        let mouse_only = Natural {
            mouse: true,
            touchpad: false,
        };
        assert!(mouse_only.turned(true));
        assert!(!mouse_only.turned(false));
        assert!(!Natural::default().turned(true));
    }

    #[test]
    fn kwin_devices_say_which_kinds_scroll_naturally() {
        // A keyboard, a mouse that does not, a touchpad that does.
        let devices = [
            (false, false, false),
            (true, false, false),
            (true, true, true),
        ];
        assert_eq!(
            natural_of_kwin(&devices),
            Natural {
                mouse: false,
                touchpad: true
            }
        );
        assert_eq!(
            natural_of_kwin(&[(true, false, true)]),
            Natural {
                mouse: true,
                touchpad: false
            }
        );
        assert_eq!(natural_of_kwin(&[]), Natural::default());
    }

    #[test]
    fn what_was_read_last_is_what_capture_gets() {
        let natural = NaturalScroll::default();
        assert_eq!(natural.get(), Natural::default());
        let read = Natural {
            mouse: true,
            touchpad: true,
        };
        natural.set(read);
        assert_eq!(natural.get(), read);
    }

    #[test]
    fn dconf_says_when_the_peripherals_settings_may_have_changed() {
        let changes = |c: &[&str]| c.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        // One key, as GSettings writes it.
        assert!(touches_peripherals(
            "/org/gnome/desktop/peripherals/touchpad/natural-scroll",
            &changes(&[""])
        ));
        // Several under a common folder.
        assert!(touches_peripherals(
            "/org/gnome/desktop/peripherals/",
            &changes(&["mouse/natural-scroll", "mouse/speed"])
        ));
        // Everything reset.
        assert!(touches_peripherals("/", &changes(&[""])));
        assert!(touches_peripherals("/org/gnome/", &changes(&[])));
        // Elsewhere.
        assert!(!touches_peripherals(
            "/org/gnome/desktop/background/picture-uri",
            &changes(&[""])
        ));
        assert!(!touches_peripherals(
            "/org/gnome/desktop/",
            &changes(&["interface/color-scheme"])
        ));
        // A key whose name merely begins like the folder.
        assert!(!touches_peripherals(
            "/org/gnome/desktop/peripherals",
            &changes(&[""])
        ));
    }
}
