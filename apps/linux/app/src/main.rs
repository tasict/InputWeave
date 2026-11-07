//! The InputWeave app for Linux: GTK4 and
//! libadwaita over the agent's IPC. It decides nothing itself; what it
//! shows comes from `inputweave_app_core`, and what it does is a command to
//! the agent.

mod about;
mod canvas;
#[cfg(test)]
mod gtk_tests;
mod license;
mod notify;
mod overlay;
mod pages;
mod pairing;
mod screenshot;
mod send;
mod setup;
mod strings;
mod style;
mod systemd;
mod transfer_row;
mod tray;
mod tray_art;
mod ui;
mod window;
mod woven;

use std::path::PathBuf;

use adw::prelude::*;
use inputweave_app_core::text::Language;
use inputweave_app_core::{APP_ID, log};

/// What the command line asks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Launch {
    /// Started by the desktop at login, or by the agent: only the tray icon.
    pub background: bool,
    /// Draw the windows with sample data into this folder, then quit.
    pub screenshots: Option<PathBuf>,
    /// The page to open, for testing.
    pub page: Option<String>,
    pub socket: Option<PathBuf>,
    /// First-run setup, whether finished or not.
    pub setup: bool,
    /// For testing: shows this in the HUD.
    pub hud: Option<String>,
    /// For testing: identifies the screens for 20 seconds.
    pub identify: bool,
    /// For testing: shows where the cursor is.
    pub locate: bool,
    /// For testing: lights this edge of the first monitor.
    pub glow: Option<inputweave_app_core::proto::ScreenEdge>,
    /// Files and folders to send, as a file manager passes them
    /// (`--send <path>…`, Dolphin's service menu).
    pub send: Option<Vec<String>>,
}

impl Launch {
    pub fn parse(args: &[String]) -> Result<(Launch, Option<Language>), String> {
        let mut launch = Launch::default();
        let mut language = None;
        let mut args = args.iter().skip(1);
        while let Some(arg) = args.next() {
            let mut value = |name: &str| {
                args.next()
                    .cloned()
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            match arg.as_str() {
                "--background" => launch.background = true,
                // GApplication's own, as when D-Bus starts the app.
                "--gapplication-service" => {}
                "--screenshot" => launch.screenshots = Some(PathBuf::from(value(arg)?)),
                "--page" => launch.page = Some(value(arg)?),
                "--socket" => launch.socket = Some(PathBuf::from(value(arg)?)),
                "--setup" => launch.setup = true,
                "--show-hud" => launch.hud = Some(value(arg)?),
                "--show-identify" => launch.identify = true,
                "--show-locator" => launch.locate = true,
                "--show-glow" => {
                    use inputweave_app_core::proto::ScreenEdge;
                    let name = value(arg)?;
                    launch.glow = Some(match name.as_str() {
                        "left" => ScreenEdge::Left,
                        "right" => ScreenEdge::Right,
                        "top" => ScreenEdge::Top,
                        "bottom" => ScreenEdge::Bottom,
                        _ => return Err(format!("no edge {name}: left, right, top or bottom")),
                    });
                }
                // Everything after it is a path, even one that starts
                // with "--".
                "--send" => {
                    let paths: Vec<String> = args.by_ref().cloned().collect();
                    if paths.is_empty() {
                        return Err("--send needs the files to send".into());
                    }
                    launch.send = Some(paths);
                }
                "--language" => {
                    let name = value(arg)?;
                    language = Some(
                        Language::parse(&name).ok_or_else(|| format!("no strings in {name}"))?,
                    );
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        Ok((launch, language))
    }
}

fn main() -> gtk::glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if options(&args).iter().skip(1).any(|a| a == "--version") {
        println!("inputweave-app {}", env!("CARGO_PKG_VERSION"));
        return gtk::glib::ExitCode::SUCCESS;
    }
    let language = match Launch::parse(&args) {
        Ok((_, language)) => language,
        Err(e) => {
            eprintln!("inputweave-app: {e}");
            return gtk::glib::ExitCode::FAILURE;
        }
    };
    strings::choose(language);
    let mut flags = gtk::gio::ApplicationFlags::HANDLES_COMMAND_LINE;
    if options(&args).iter().any(|a| a == "--screenshot") {
        // Screenshots run on their own, next to a running app.
        flags |= gtk::gio::ApplicationFlags::NON_UNIQUE;
    }
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(flags)
        .build();
    app.connect_startup(|app| {
        style::install();
        // For whatever starts the app over D-Bus, as the agent may to have
        // it hold a question for the user: running, with only its tray icon.
        let background = gtk::gio::SimpleAction::new("background", None);
        let weak = app.downgrade();
        background.connect_activate(move |_, _| {
            if let Some(app) = weak.upgrade() {
                ui::launched(
                    &app,
                    Launch {
                        background: true,
                        ..Launch::default()
                    },
                    false,
                );
            }
        });
        app.add_action(&background);
    });
    // Activated over D-Bus, or from a desktop entry that does so.
    app.connect_activate(|app| ui::launched(app, Launch::default(), false));
    app.connect_command_line(|app, command_line| {
        let args: Vec<String> = command_line
            .arguments()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        match Launch::parse(&args) {
            Ok((mut launch, _)) => {
                // Relative to where the command was run, which for another
                // launch handing over is not this process's folder.
                if let Some(paths) = &mut launch.send {
                    let cwd = command_line.cwd();
                    for path in paths.iter_mut() {
                        *path = resolve(cwd.as_deref(), path);
                    }
                }
                ui::launched(app, launch, command_line.is_remote());
                gtk::glib::ExitCode::SUCCESS
            }
            Err(e) => {
                log::info(&format!("ignoring the command line: {e}"));
                gtk::glib::ExitCode::FAILURE
            }
        }
    });
    app.run_with_args(&args)
}

/// The arguments up to `--send`, after which every one is a path to send
/// (a file may be called "--version").
fn options(args: &[String]) -> &[String] {
    let end = args
        .iter()
        .position(|a| a == "--send")
        .unwrap_or(args.len());
    &args[..end]
}

/// `path` as an absolute path, a relative one taken from `cwd`.
fn resolve(cwd: Option<&std::path::Path>, path: &str) -> String {
    let given = std::path::Path::new(path);
    match cwd {
        Some(cwd) if given.is_relative() => cwd.join(given).to_string_lossy().into_owned(),
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        std::iter::once("inputweave-app")
            .chain(list.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn the_command_line() {
        assert_eq!(Launch::parse(&args(&[])).unwrap().0, Launch::default());
        let (launch, language) =
            Launch::parse(&args(&["--background", "--language", "zh-TW"])).unwrap();
        assert!(launch.background);
        assert_eq!(language, Some(Language::ChineseTaiwan));
        let (launch, _) =
            Launch::parse(&args(&["--screenshot", "/tmp/s", "--page", "files"])).unwrap();
        assert_eq!(
            launch.screenshots.as_deref(),
            Some(std::path::Path::new("/tmp/s"))
        );
        assert_eq!(launch.page.as_deref(), Some("files"));
        assert_eq!(
            Launch::parse(&args(&["--gapplication-service"])).unwrap().0,
            Launch::default(),
            "GApplication's own"
        );
        assert!(Launch::parse(&args(&["--nope"])).is_err());
        assert!(Launch::parse(&args(&["--socket"])).is_err());
        assert!(Launch::parse(&args(&["--language", "fr"])).is_err());
        assert_eq!(
            Launch::parse(&args(&["--show-glow", "top"]))
                .unwrap()
                .0
                .glow,
            Some(inputweave_app_core::proto::ScreenEdge::Top)
        );
        assert!(Launch::parse(&args(&["--show-glow", "middle"])).is_err());
    }

    #[test]
    fn send_takes_every_path_after_it() {
        let (launch, _) = Launch::parse(&args(&[
            "--send",
            "/home/me/Report.pdf",
            "--background",
            "Photos",
        ]))
        .unwrap();
        assert_eq!(
            launch.send.as_deref(),
            Some(
                [
                    "/home/me/Report.pdf".to_owned(),
                    "--background".to_owned(),
                    "Photos".to_owned()
                ]
                .as_slice()
            ),
            "a path that looks like an option is still a path"
        );
        assert!(!launch.background);
        assert!(
            Launch::parse(&args(&["--send"])).is_err(),
            "nothing to send"
        );
    }

    #[test]
    fn options_end_at_send() {
        let all = args(&["--language", "en", "--send", "--version", "--screenshot"]);
        assert_eq!(options(&all), &all[..3]);
        assert!(!options(&all).iter().any(|a| a == "--version"));
        let plain = args(&["--version"]);
        assert_eq!(options(&plain), plain.as_slice());
    }

    #[test]
    fn relative_paths_are_taken_from_where_the_command_ran() {
        let cwd = std::path::Path::new("/home/me/Downloads");
        assert_eq!(
            resolve(Some(cwd), "Report.pdf"),
            "/home/me/Downloads/Report.pdf"
        );
        assert_eq!(resolve(Some(cwd), "/tmp/a b"), "/tmp/a b");
        assert_eq!(resolve(None, "Report.pdf"), "Report.pdf");
    }
}
