//! The five brand colors, light and dark; every
//! other color is the system's. libadwaita would load style-dark.css from
//! the app's resources, which this app does not ship, so the two sets are
//! swapped as the style manager turns dark.

/// The five brand colors of one appearance, as red, green, blue from 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Brand {
    pub thread: [f64; 3],
    pub canvas: [f64; 3],
    pub grid: [f64; 3],
    pub tile: [f64; 3],
    pub tile_edge: [f64; 3],
    pub dark: bool,
}

const fn rgb(hex: u32) -> [f64; 3] {
    [
        ((hex >> 16) & 0xFF) as f64 / 255.0,
        ((hex >> 8) & 0xFF) as f64 / 255.0,
        (hex & 0xFF) as f64 / 255.0,
    ]
}

pub const LIGHT: Brand = Brand {
    thread: rgb(0x8E3FD8),
    canvas: rgb(0xECEDF1),
    grid: rgb(0xD3D5DC),
    tile: rgb(0xFFFFFF),
    tile_edge: rgb(0xC4C7D0),
    dark: false,
};

pub const DARK: Brand = Brand {
    thread: rgb(0xBF8CFF),
    canvas: rgb(0x18191D),
    grid: rgb(0x2C2E35),
    tile: rgb(0x2A2C33),
    tile_edge: rgb(0x41444E),
    dark: true,
};

/// The colors for the style manager's appearance now.
pub fn brand() -> Brand {
    if adw::StyleManager::default().is_dark() {
        DARK
    } else {
        LIGHT
    }
}

fn css(brand: &Brand) -> String {
    let hex = |c: [f64; 3]| {
        format!(
            "#{:02X}{:02X}{:02X}",
            (c[0] * 255.0).round() as u8,
            (c[1] * 255.0).round() as u8,
            (c[2] * 255.0).round() as u8
        )
    };
    format!(
        "@define-color iw_thread {};\n@define-color iw_canvas {};\n@define-color iw_grid {};\n@define-color iw_tile {};\n@define-color iw_tile_edge {};\n",
        hex(brand.thread),
        hex(brand.canvas),
        hex(brand.grid),
        hex(brand.tile),
        hex(brand.tile_edge)
    )
}

const RULES: &str = "
.iw-code { font-size: 48pt; font-weight: 600; font-feature-settings: \"tnum\"; letter-spacing: 0.1em; }
.iw-code-digit { font-size: 20pt; font-feature-settings: \"tnum\"; }
.iw-numeric { font-feature-settings: \"tnum\"; }
.iw-here { color: @iw_thread; }
.iw-shortcut { font-feature-settings: \"tnum\"; }
@keyframes iw-shake {
  0% { transform: translateX(0); }
  20% { transform: translateX(-8px); }
  40% { transform: translateX(8px); }
  60% { transform: translateX(-5px); }
  80% { transform: translateX(5px); }
  100% { transform: translateX(0); }
}
.iw-shake { animation: iw-shake 0.4s ease-in-out; }
.iw-recording { border: 2px solid @accent_bg_color; border-radius: 6px; padding: 2px 8px; }
window.iw-identify, window.iw-identify.background { background: none; box-shadow: none; }
row.iw-failed .subtitle { color: @error_color; opacity: 1; }
row.iw-command .subtitle { font-family: monospace; }
.iw-version { color: @accent_color; background: alpha(@accent_bg_color, 0.12); border-radius: 999px; padding: 2px 12px; font-weight: 700; }
button.iw-remove { color: @destructive_color; }
.iw-trial-pill { font-size: 0.85em; padding: 1px 8px; border-radius: 10px; background: alpha(currentColor, 0.08); }
.iw-capsule { font-size: 0.85em; padding: 1px 8px; border-radius: 999px; background: alpha(currentColor, 0.08); }
.iw-step { min-width: 24px; min-height: 24px; border-radius: 12px; background: alpha(currentColor, 0.07); font-weight: 700; }
label.iw-license-notice { padding: 10px 14px; background: alpha(@warning_color, 0.16); border-radius: 10px; box-shadow: none; }
";

pub fn install() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    let manager = adw::StyleManager::default();
    let load = {
        let provider = provider.clone();
        move |dark: bool| {
            let colors = css(if dark { &DARK } else { &LIGHT });
            provider.load_from_string(&format!("{colors}{RULES}"));
        }
    };
    load(manager.is_dark());
    manager.connect_dark_notify(move |m| load(m.is_dark()));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
