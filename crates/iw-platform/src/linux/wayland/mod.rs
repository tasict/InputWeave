//! Wayland backends: the displays from
//! `wl_output` and `xdg_output`, input from other devices through the
//! desktop's RemoteDesktop portal and libei, this device's keyboard
//! and mouse captured through the InputCapture portal and libei, and
//! the clipboard through data-control where the compositor offers it
//! (KDE Plasma, wlroots compositors), else through the Clipboard portal
//! with remote control (GNOME). Compositors without those portals (sway,
//! Hyprland and other wlroots ones) get input through their own protocols
//! instead: a virtual pointer and keyboard (`wlr_emulation`) and
//! layer surfaces at the exits (`wlr_capture`); which way is the rule in
//! `wlroots`, decided once from the compositor's globals.
//!
//! The pure parts (`translate`, `tokens`, `barriers`, `clipboard`,
//! `wlroots`, `modifiers`) are unit tested on every OS (`clipboard` and
//! `modifiers` on every Unix); the rest is tested in the Linux container
//! against a fake portal and libei server, and against headless sway
//! (`scripts/linux-wayland-tests.sh`).
//! Nothing has been tried on a real GNOME, KDE, sway or Hyprland desktop
//! yet.

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod barriers;
#[cfg(any(target_os = "linux", all(test, unix)))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod clipboard;
#[cfg(any(target_os = "linux", all(test, unix)))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod modifiers;
pub(crate) mod tokens;
pub(crate) mod translate;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod wlroots;

#[cfg(target_os = "linux")]
mod capture;
#[cfg(target_os = "linux")]
mod data_control;
#[cfg(target_os = "linux")]
mod displays;
#[cfg(target_os = "linux")]
mod outputs;
#[cfg(target_os = "linux")]
mod portal_clipboard;
#[cfg(target_os = "linux")]
mod portal_session;
#[cfg(target_os = "linux")]
mod remote;
#[cfg(all(test, target_os = "linux"))]
mod tests;
#[cfg(target_os = "linux")]
mod wlr;
#[cfg(target_os = "linux")]
mod wlr_capture;
#[cfg(target_os = "linux")]
mod wlr_emulation;

#[cfg(target_os = "linux")]
pub use self::{
    capture::PortalCapture, data_control::DataControlClipboard, displays::WaylandDisplays,
    portal_clipboard::PortalClipboard, wlr_capture::WlrCapture, wlr_emulation::WlrEmulation,
};

/// The app id the portals know InputWeave by: the app's, whose desktop file
/// names it "InputWeave" in the desktop's dialogs. The agent's systemd unit
/// is named so xdg-desktop-portal reads this id from it
/// (`app-cc.ctsite.inputweave@agent.service`), and newer portals are told
/// it outright.
pub const APP_ID: &str = "cc.ctsite.inputweave";

#[cfg(target_os = "linux")]
mod platform {
    use std::sync::Arc;

    use iw_core::HidUsage;
    use iw_core::geometry::Point;
    use iw_core::input::Button;

    use super::capture::{Capture, CaptureOptions};
    use super::clipboard::{Chosen, LetsGo, WaylandClipboard, Which};
    use super::remote::{Input, Remote, RemoteOptions};
    use super::tokens::Tokens;
    use super::translate::{Held, ei_button, portal_backend, scroll};
    use super::wlr_capture::Edges;
    use super::wlr_emulation::Reach;
    use super::wlroots::{self, Way};
    use crate::linux::info::Info;
    use crate::linux::natural_scroll::NaturalScroll;
    use crate::linux::screen_lock::ScreenLock;
    use crate::{
        Backend, BackendRole, Clipboard, ClipboardContent, ClipboardPromise, ClipboardRead,
        DesktopPictures, InputCapture, InputEmulation, Native, NativeOptions, Permission,
        PermissionState, PlatformError, SessionInfo, SessionStatus,
    };

    pub(crate) fn native(
        options: &NativeOptions,
        info: &Info,
        pictures: Box<dyn DesktopPictures>,
    ) -> Result<Native, PlatformError> {
        let displays = super::WaylandDisplays::connect()?;
        let tokens = Tokens::in_dir(options.state_dir.as_deref());
        let offers = displays.offers();
        let (injection, capturing) = ways(offers);
        let mut remote = RemoteOptions::new(tokens.clone(), super::APP_ID);
        // Every remote desktop session asks for the portal's clipboard;
        // it is used where the compositor turns out to have no
        // data-control. Without that session there is no portal clipboard
        // either: on wlroots without data-control, as the globals tell at
        // once, there is no clipboard.
        let fallback: Option<Box<dyn LetsGo>> = match injection {
            Way::Portal => {
                let portal = super::PortalClipboard::new();
                remote.clipboard = Some(portal.portal());
                Some(Box::new(portal))
            }
            Way::Wlroots if offers.data_control => Some(Box::new(NoPortalClipboard::default())),
            Way::Wlroots => {
                tracing::warn!("no clipboard sharing: {NO_CLIPBOARD}");
                None
            }
        };
        let clipboard = fallback.map(|fallback| {
            WaylandClipboard::new(Box::new(super::DataControlClipboard::new()), fallback)
        });
        let which = clipboard.as_ref().map(WaylandClipboard::which);
        let (emulation, input): (Box<dyn InputEmulation>, Reaches) = match injection {
            Way::Portal => {
                let remote = Arc::new(Remote::start(remote)?);
                (
                    Box::new(PortalEmulation::new(remote.clone())),
                    Reaches::Portal(remote),
                )
            }
            Way::Wlroots => {
                let emulation = super::WlrEmulation::start()?;
                let reach = emulation.reach();
                (Box::new(emulation), Reaches::Wlroots(reach))
            }
        };
        let (capture, captures): (Box<dyn InputCapture>, Captures) = match capturing {
            Way::Portal => {
                let mut options = CaptureOptions::new(tokens, super::APP_ID);
                options.natural = NaturalScroll::watch(info.desktop());
                let capture = Arc::new(Capture::start(options)?);
                (
                    Box::new(super::PortalCapture::new(capture.clone())),
                    Captures::Portal(capture),
                )
            }
            Way::Wlroots => {
                let capture = super::WlrCapture::start()?;
                let edges = capture.edges();
                (Box::new(capture), Captures::Edges(edges))
            }
        };
        Ok(Native {
            capture,
            emulation,
            displays: Box::new(displays),
            pictures,
            status: Box::new(
                WaylandStatus::new(input)
                    .capturing(captures)
                    .described(info.clone(), which),
            ),
            clipboard: clipboard.map(|c| Box::new(c) as Box<dyn Clipboard>),
        })
    }

    /// How input goes in and out of this desktop (`wlroots::injection`,
    /// `wlroots::capture`): decided once, from the compositor's globals
    /// alone, as the displays' connection listed them (without it the
    /// agent does not start). Said once in the log.
    fn ways(globals: wlroots::Globals) -> (Way, Way) {
        let ways = (wlroots::injection(&globals), wlroots::capture(&globals));
        tracing::info!(
            ?globals,
            injection = ?ways.0,
            capture = ?ways.1,
            "how input goes in and out of this Wayland desktop"
        );
        ways
    }

    /// Why there is no clipboard on a compositor without data-control,
    /// where no remote desktop session runs to share the portal's: in the
    /// log, and to the app.
    const NO_CLIPBOARD: &str = "the compositor offers no data-control and no remote desktop portal";

    /// The clipboard's fallback on wlroots, behind the data-control its
    /// globals list, where no remote desktop session runs to share the
    /// portal's: should data-control say it is not there after all
    /// (`Unsupported`), there is no clipboard, said once in the log, and a
    /// promise handed over is finished.
    #[derive(Default)]
    struct NoPortalClipboard {
        said: std::sync::atomic::AtomicBool,
    }

    impl NoPortalClipboard {
        fn unsupported(&self) -> PlatformError {
            if !self.said.swap(true, std::sync::atomic::Ordering::SeqCst) {
                tracing::warn!("no clipboard sharing: {NO_CLIPBOARD}");
            }
            PlatformError::Unsupported(NO_CLIPBOARD)
        }
    }

    impl Clipboard for NoPortalClipboard {
        fn change_count(&self) -> Result<u64, PlatformError> {
            Err(self.unsupported())
        }

        fn read(&self, _: usize) -> Result<ClipboardRead, PlatformError> {
            Err(self.unsupported())
        }

        fn write(&mut self, _: &ClipboardContent) -> Result<u64, PlatformError> {
            Err(self.unsupported())
        }

        /// The promise is gone, as the clipboard that took it tells it.
        fn write_promise(&mut self, promise: ClipboardPromise) -> Result<u64, PlatformError> {
            promise.source.finished();
            Err(self.unsupported())
        }

        fn files(&self) -> Result<Vec<std::path::PathBuf>, PlatformError> {
            Err(self.unsupported())
        }
    }

    impl LetsGo for NoPortalClipboard {
        fn let_go(&mut self) {}
    }

    /// How input from other devices reaches the desktop.
    pub(crate) enum Reaches {
        Portal(Arc<Remote>),
        Wlroots(Reach),
    }

    /// How this device's input is captured.
    pub(crate) enum Captures {
        Portal(Arc<Capture>),
        Edges(Edges),
    }

    /// Input from other devices, through the RemoteDesktop portal's libei
    /// connection. Until the desktop allowed it, every call is
    /// `Unavailable`.
    ///
    /// libei tells nothing of where the pointer is, so `position` has no
    /// answer: as relative mode ends here, the controlling device carries
    /// on from where it began, after its short wait.
    pub struct PortalEmulation {
        remote: Arc<Remote>,
        keys: Held,
        buttons: Held,
        /// The session `held` is of.
        session: u64,
    }

    impl PortalEmulation {
        pub(crate) fn new(remote: Arc<Remote>) -> Self {
            Self {
                session: remote.sessions(),
                remote,
                keys: Held::default(),
                buttons: Held::default(),
            }
        }

        /// Nothing is held down in a session that ran after the one the
        /// keys went down in.
        fn follow_session(&mut self) {
            let now = self.remote.sessions();
            if now != self.session {
                self.session = now;
                self.keys = Held::default();
                self.buttons = Held::default();
            }
        }

        fn key_press(&mut self, code: u32, pressed: bool) -> Result<(), PlatformError> {
            self.follow_session();
            press(&self.remote, &mut self.keys, code, pressed, Input::Key)
        }

        fn button_press(&mut self, code: u32, pressed: bool) -> Result<(), PlatformError> {
            self.follow_session();
            press(
                &self.remote,
                &mut self.buttons,
                code,
                pressed,
                Input::Button,
            )
        }
    }

    /// Sends the press or release of `code`, made into input by `input`,
    /// unless `held` says it is a press of what is down already.
    fn press(
        remote: &Remote,
        held: &mut Held,
        code: u32,
        pressed: bool,
        input: fn(u32, bool) -> Input,
    ) -> Result<(), PlatformError> {
        if !held.sends(code, pressed) {
            return Ok(());
        }
        remote.send(input(code, pressed))?;
        held.sent(code, pressed);
        Ok(())
    }

    impl InputEmulation for PortalEmulation {
        fn move_to(&mut self, position: Point) -> Result<(), PlatformError> {
            self.remote.send(Input::MoveTo(position))
        }

        fn button(&mut self, button: Button, pressed: bool) -> Result<(), PlatformError> {
            self.button_press(ei_button(button), pressed)
        }

        fn key(&mut self, usage: HidUsage, pressed: bool) -> Result<(), PlatformError> {
            let code = u32::from(
                usage
                    .to_evdev()
                    .ok_or(PlatformError::Unsupported("a key Linux has no code for"))?,
            );
            self.key_press(code, pressed)
        }

        fn scroll(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError> {
            match scroll(dx, dy) {
                Some(scroll) => self.remote.send(Input::Scroll(scroll)),
                None => Ok(()),
            }
        }

        fn move_by(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError> {
            self.remote.send(Input::MoveBy(dx, dy))
        }
    }

    impl Drop for PortalEmulation {
        /// Lets go of what is held; the session itself does too as it
        /// ends, once the last of the backends holding it is dropped.
        fn drop(&mut self) {
            self.follow_session();
            for code in self.keys.take() {
                let _ = self.remote.send(Input::Key(code, false));
            }
            for code in self.buttons.take() {
                let _ = self.remote.send(Input::Button(code, false));
            }
        }
    }

    /// The session: the portals' permissions, and the screen lock. Input
    /// through wlroots' virtual devices and layer surfaces asks nobody, so
    /// it has no permission to list.
    pub struct WaylandStatus {
        input: Reaches,
        capture: Option<Captures>,
        lock: ScreenLock,
        /// The session and which clipboard is used (none on wlroots
        /// without data-control), for the app (`session_info`); `None` for
        /// one made alone, as in tests.
        described: Option<(Info, Option<Arc<Which>>)>,
    }

    impl WaylandStatus {
        /// Input from other devices as `input` says.
        pub(crate) fn new(input: Reaches) -> Self {
            Self {
                input,
                capture: None,
                lock: ScreenLock::start(),
                described: None,
            }
        }

        /// This device's input captured as `capture` says too.
        pub(crate) fn capturing(self, capture: Captures) -> Self {
            Self {
                capture: Some(capture),
                ..self
            }
        }

        /// Telling the app about `info`'s session, whose clipboard is the
        /// one `which` says, if any.
        pub(crate) fn described(self, info: Info, which: Option<Arc<Which>>) -> Self {
            Self {
                described: Some((info, which)),
                ..self
            }
        }

        fn portal_capture(&self) -> Option<&Capture> {
            match &self.capture {
                Some(Captures::Portal(capture)) => Some(capture),
                _ => None,
            }
        }
    }

    impl SessionStatus for WaylandStatus {
        /// The portals' alone: wlroots' protocols ask nobody, as the
        /// compositor hands them to every client.
        fn permissions(&self) -> Vec<PermissionState> {
            let mut all = Vec::new();
            if let Reaches::Portal(remote) = &self.input {
                all.push(remote.permission(Permission::RemoteControl));
            }
            all.extend(self.portal_capture().map(Capture::permission));
            all
        }

        fn screen_locked(&self) -> bool {
            self.lock.locked()
        }

        /// GNOME and KDE send a remote desktop session's input to their
        /// lock screens as they do to the desktop, which is how their own
        /// remote desktop sharing unlocks them. Not yet tried with
        /// InputWeave. A wlroots lock screen (ext-session-lock) is a client
        /// of its own that virtual devices' input may well reach, but the
        /// cursor is kept off rather than tried.
        fn lock_screen_input(&self) -> bool {
            matches!(self.input, Reaches::Portal(_))
        }

        fn request_permission(&self, permission: Permission) -> Result<u64, PlatformError> {
            match (permission, &self.input, self.portal_capture()) {
                (Permission::RemoteControl, Reaches::Portal(remote), _) => remote.ask(),
                (Permission::InputCapture, _, Some(capture)) => capture.ask(),
                _ => Err(PlatformError::Unsupported(
                    "asking for this permission on this Wayland desktop",
                )),
            }
        }

        /// Until the desktop allows remote control, or the compositor's
        /// virtual devices are there, other devices keep the cursor off
        /// this one.
        fn input_available(&self) -> bool {
            match &self.input {
                Reaches::Portal(remote) => remote.running(),
                Reaches::Wlroots(reach) => reach.connected(),
            }
        }

        fn session_info(&self) -> Option<SessionInfo> {
            let (info, which) = self.described.as_ref()?;
            // Each session read once: whether it runs is its permission
            // being granted, read under the same lock.
            let mut remote_runs = false;
            let mut backends = vec![match &self.input {
                Reaches::Portal(remote) => {
                    let permission = remote.permission(Permission::RemoteControl);
                    remote_runs = permission.granted;
                    portal_backend(
                        BackendRole::Input,
                        "RemoteDesktop portal",
                        &permission,
                        remote.sessions() > 0,
                    )
                }
                Reaches::Wlroots(reach) if reach.connected() => {
                    Backend::serving(BackendRole::Input, VIRTUAL_DEVICES)
                }
                Reaches::Wlroots(_) => Backend::held_back(
                    BackendRole::Input,
                    VIRTUAL_DEVICES,
                    "the compositor's virtual devices are not connected",
                ),
            }];
            match &self.capture {
                Some(Captures::Portal(capture)) => backends.push(portal_backend(
                    BackendRole::Capture,
                    "InputCapture portal",
                    &capture.permission(),
                    capture.sessions() > 0,
                )),
                Some(Captures::Edges(edges)) => backends.push(edge_backend(edges)),
                None => {}
            }
            backends.push(match which.as_deref().map(Which::get) {
                Some(Chosen::DataControl) => {
                    Backend::serving(BackendRole::Clipboard, "data-control")
                }
                Some(Chosen::Portal) if remote_runs => {
                    Backend::serving(BackendRole::Clipboard, "Clipboard portal")
                }
                Some(Chosen::Portal) if matches!(self.input, Reaches::Portal(_)) => {
                    Backend::held_back(
                        BackendRole::Clipboard,
                        "Clipboard portal",
                        "only while remote control runs",
                    )
                }
                // No data-control, and no remote desktop session to share
                // the portal's.
                Some(Chosen::Portal) | None => {
                    Backend::missing(BackendRole::Clipboard, NO_CLIPBOARD)
                }
                // Chosen as the clipboard is first used (as the cursor
                // first crosses, with sharing it on): nothing is wrong yet.
                Some(Chosen::Unknown) => Backend::serving(BackendRole::Clipboard, "not used yet"),
            });
            backends.push(Backend::serving(BackendRole::Displays, "wl_output"));
            Some(info.with(backends))
        }
    }

    /// The backends without portals, as the app is told of them.
    const VIRTUAL_DEVICES: &str = "wlroots virtual pointer and keyboard";
    const EDGES: &str = "wlroots layer-shell edges";

    /// Capture at layer surfaces, served while its connection runs.
    fn edge_backend(edges: &Edges) -> Backend {
        if edges.connected() {
            Backend::serving(BackendRole::Capture, EDGES)
        } else {
            Backend::held_back(
                BackendRole::Capture,
                EDGES,
                "the compositor's layer shell is not connected",
            )
        }
    }

    #[cfg(test)]
    mod tests {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use super::*;

        #[derive(Default)]
        struct Source(AtomicUsize);

        impl crate::ClipboardSource for Source {
            fn fetch(&self, _: crate::ClipboardFormat) -> Option<Vec<u8>> {
                None
            }

            fn finished(&self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        #[test]
        fn without_a_portal_session_there_is_no_clipboard_to_wait_for() {
            let mut clipboard = NoPortalClipboard::default();
            let unsupported = |r: Result<u64, PlatformError>| {
                matches!(
                    r,
                    Err(PlatformError::Unsupported(why)) if why.contains("no data-control")
                )
            };
            assert!(unsupported(clipboard.change_count()));
            assert!(unsupported(clipboard.write(&ClipboardContent::default())));
            assert!(clipboard.read(1024).is_err());
            assert!(clipboard.files().is_err());
            // A promise handed to it is told it is gone.
            let source = Arc::new(Source::default());
            let promise = ClipboardPromise {
                formats: vec![crate::ClipboardFormat::Text],
                bytes: 1,
                files: Vec::new(),
                source: source.clone(),
            };
            assert!(unsupported(clipboard.write_promise(promise)));
            assert_eq!(source.0.load(Ordering::SeqCst), 1);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) use platform::Reaches;
#[cfg(target_os = "linux")]
pub(crate) use platform::native;
#[cfg(target_os = "linux")]
pub use platform::{PortalEmulation, WaylandStatus};
