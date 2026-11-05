//! The tray icon: a StatusNotifierItem with a `com.canonical.dbusmenu`
//! menu, on the session bus, which KDE Plasma shows natively and GNOME
//! through the AppIndicator extension (Ubuntu turns it on). Its D-Bus
//! connection runs on zbus's own runtime, which a helper thread drives
//! whenever the GTK thread is not waiting on it in `zbus::block_on`; menu
//! clicks reach the GTK main loop through a channel.
//!
//! The icon comes two ways: by name, from symbolic SVGs the app writes to
//! an icon theme folder of its own (`IconThemePath`), which panels tint
//! like their other icons; and as pixmaps for panels that ignore the name.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use inputweave_app_core::log;
use inputweave_app_core::status_text::TrayState;
use inputweave_app_core::tray_icon;
use zbus::export::futures_core::Stream;
use zbus::object_server::SignalEmitter;
use zbus::{Connection, OwnedVariant, Variant, fdo, interface};

/// Icons of several sizes: width, height, ARGB32 in network byte order.
pub type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

/// An icon name, its pixmaps, a title and a description.
type ToolTip = (String, Pixmaps, String, String);

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

/// What a menu item does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Open,
    SendFiles,
    ToggleSharing,
    ToggleLock,
    IdentifyScreens,
    AddDevice,
    Settings,
    RunSetup,
    About,
    Quit,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MenuItem {
    pub label: String,
    pub enabled: bool,
    pub action: Option<TrayAction>,
}

impl MenuItem {
    pub fn separator() -> MenuItem {
        MenuItem {
            label: String::new(),
            enabled: true,
            action: None,
        }
    }

    fn is_separator(&self) -> bool {
        self.action.is_none() && self.label.is_empty()
    }
}

/// What the tray shows.
#[derive(Clone, Debug, PartialEq)]
pub struct TrayModel {
    pub state: TrayState,
    /// The tooltip: what screen readers say, and the status line.
    pub description: String,
    pub detail: String,
    pub menu: Vec<MenuItem>,
}

struct Shared {
    model: TrayModel,
    revision: u32,
    pixmaps: HashMap<TrayState, Pixmaps>,
    theme_path: String,
    actions: async_channel::Sender<TrayAction>,
}

type State = Arc<Mutex<Shared>>;

fn lock(state: &State) -> std::sync::MutexGuard<'_, Shared> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

struct Item {
    state: State,
}

#[interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }

    #[zbus(property)]
    fn id(&self) -> &str {
        inputweave_app_core::APP_ID
    }

    #[zbus(property)]
    fn title(&self) -> &str {
        "InputWeave"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        if lock(&self.state).model.state == TrayState::Attention {
            "NeedsAttention"
        } else {
            "Active"
        }
    }

    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }

    #[zbus(property)]
    fn icon_name(&self) -> String {
        tray_icon::icon_name(lock(&self.state).model.state)
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Pixmaps {
        let shared = lock(&self.state);
        shared
            .pixmaps
            .get(&shared.model.state)
            .cloned()
            .unwrap_or_default()
    }

    #[zbus(property)]
    fn attention_icon_name(&self) -> String {
        tray_icon::icon_name(TrayState::Attention)
    }

    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Pixmaps {
        lock(&self.state)
            .pixmaps
            .get(&TrayState::Attention)
            .cloned()
            .unwrap_or_default()
    }

    #[zbus(property)]
    fn overlay_icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn overlay_icon_pixmap(&self) -> Pixmaps {
        Vec::new()
    }

    #[zbus(property)]
    fn attention_movie_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> String {
        lock(&self.state).theme_path.clone()
    }

    #[zbus(property)]
    fn tool_tip(&self) -> ToolTip {
        let shared = lock(&self.state);
        (
            tray_icon::icon_name(shared.model.state),
            Vec::new(),
            shared.model.description.clone(),
            shared.model.detail.clone(),
        )
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn menu(&self) -> zbus::OwnedObjectPath {
        zbus::OwnedObjectPath::try_from(MENU_PATH).expect("a valid path")
    }

    fn activate(&self, _x: i32, _y: i32) {
        let _ = lock(&self.state).actions.try_send(TrayAction::Open);
    }

    fn secondary_activate(&self, _x: i32, _y: i32) {
        let _ = lock(&self.state).actions.try_send(TrayAction::Open);
    }

    fn context_menu(&self, _x: i32, _y: i32) {}

    fn scroll(&self, _delta: i32, _orientation: String) {}

    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_attention_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_status(emitter: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;
}

/// One item's properties as dbusmenu wants them.
type Properties = HashMap<String, OwnedVariant>;
type Layout = (i32, Properties, Vec<OwnedVariant>);

fn owned(value: Variant<'_>) -> OwnedVariant {
    OwnedVariant::try_from(value).expect("plain values hold no file descriptors")
}

fn properties(item: &MenuItem) -> Properties {
    let mut props = Properties::new();
    if item.is_separator() {
        props.insert("type".into(), owned(Variant::from("separator")));
        return props;
    }
    props.insert(
        "label".into(),
        owned(Variant::from(item.label.replace('_', "__"))),
    );
    props.insert(
        "enabled".into(),
        owned(Variant::from(item.enabled && item.action.is_some())),
    );
    props
}

/// Menu item ids are their place plus one; 0 is the root.
fn layout(menu: &[MenuItem]) -> Layout {
    let children = menu
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let child: Layout = (i as i32 + 1, properties(item), Vec::new());
            owned(Variant::from(child))
        })
        .collect();
    let mut root = Properties::new();
    root.insert("children-display".into(), owned(Variant::from("submenu")));
    (0, root, children)
}

struct Menu {
    state: State,
}

#[interface(name = "com.canonical.dbusmenu")]
impl Menu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        vec![lock(&self.state).theme_path.clone()]
    }

    fn get_layout(
        &self,
        parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> fdo::Result<(u32, Layout)> {
        let shared = lock(&self.state);
        let full = layout(&shared.model.menu);
        if parent_id == 0 {
            return Ok((shared.revision, full));
        }
        let index = parent_id as usize;
        match shared.model.menu.get(index.wrapping_sub(1)) {
            Some(item) => Ok((shared.revision, (parent_id, properties(item), Vec::new()))),
            None => Err(fdo::Error::InvalidArgs(format!("no item {parent_id}"))),
        }
    }

    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        _property_names: Vec<String>,
    ) -> Vec<(i32, Properties)> {
        let shared = lock(&self.state);
        let menu = &shared.model.menu;
        let all: Vec<i32> = (1..=menu.len() as i32).collect();
        let ids = if ids.is_empty() { all } else { ids };
        ids.into_iter()
            .filter_map(|id| {
                menu.get((id as usize).wrapping_sub(1))
                    .map(|item| (id, properties(item)))
            })
            .collect()
    }

    fn get_property(&self, id: i32, name: String) -> fdo::Result<OwnedVariant> {
        let shared = lock(&self.state);
        shared
            .model
            .menu
            .get((id as usize).wrapping_sub(1))
            .and_then(|item| properties(item).remove(&name))
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("no {name} on {id}")))
    }

    fn event(&self, id: i32, event_id: String, _data: OwnedVariant, _timestamp: u32) {
        if event_id != "clicked" {
            return;
        }
        let shared = lock(&self.state);
        if let Some(MenuItem {
            action: Some(action),
            enabled: true,
            ..
        }) = shared.model.menu.get((id as usize).wrapping_sub(1))
        {
            let _ = shared.actions.try_send(*action);
        }
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedVariant, u32)>) -> Vec<i32> {
        for (id, event_id, data, timestamp) in events {
            self.event(id, event_id, data, timestamp);
        }
        Vec::new()
    }

    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }

    #[zbus(signal)]
    async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;
}

/// The tray icon while the app runs.
pub struct Tray {
    connection: Connection,
    state: State,
    name: String,
}

impl Tray {
    /// Puts the icon on the session bus and registers it with the panel's
    /// watcher. Returns `None` when there is no session bus.
    pub fn start(
        model: TrayModel,
        pixmaps: HashMap<TrayState, Pixmaps>,
        theme_path: Option<PathBuf>,
        actions: async_channel::Sender<TrayAction>,
    ) -> Option<Tray> {
        let state: State = Arc::new(Mutex::new(Shared {
            model,
            revision: 1,
            pixmaps,
            theme_path: theme_path
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            actions,
        }));
        let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
        let connection = zbus::block_on(
            zbus::connection::Builder::session()
                .name(name.as_str())
                .serve_at(
                    ITEM_PATH,
                    Item {
                        state: state.clone(),
                    },
                )
                .serve_at(
                    MENU_PATH,
                    Menu {
                        state: state.clone(),
                    },
                )
                .build(),
        );
        let connection = match connection {
            Ok(c) => c,
            Err(e) => {
                log::info(&format!("no tray icon: {e}"));
                return None;
            }
        };
        let tray = Tray {
            connection,
            state,
            name,
        };
        tray.register();
        tray.watch_for_watchers();
        Some(tray)
    }

    /// Whether a panel shows tray icons now.
    pub fn shown(&self) -> bool {
        zbus::block_on(async {
            let proxy = fdo::DBusProxy::new(&self.connection).await.ok()?;
            let name = zbus::names::BusName::try_from(WATCHER).ok()?;
            proxy.name_has_owner(name).await.ok()
        })
        .unwrap_or(false)
    }

    fn register(&self) {
        let result = zbus::block_on(async {
            zbus::Proxy::new(&self.connection, WATCHER, "/StatusNotifierWatcher", WATCHER)
                .await?
                .call::<_, _, ()>("RegisterStatusNotifierItem", &(self.name.as_str(),))
                .await
        });
        match result {
            Ok(()) => log::info("tray icon registered"),
            Err(e) => log::info(&format!("no panel shows tray icons now: {e}")),
        }
    }

    /// Registers again whenever a watcher starts, as when the shell
    /// restarts or the AppIndicator extension is turned on: on a thread
    /// that waits for that, all along, in `zbus::block_on`.
    fn watch_for_watchers(&self) {
        let connection = self.connection.clone();
        let name = self.name.clone();
        std::thread::Builder::new()
            .name("tray-watcher".into())
            .spawn(move || {
                zbus::block_on(async {
                    let Ok(proxy) = fdo::DBusProxy::new(&connection).await else {
                        return;
                    };
                    let Ok(changes) = proxy
                        .receive_name_owner_changed_with_args(&[(0, WATCHER)])
                        .await
                    else {
                        return;
                    };
                    let mut changes = std::pin::pin!(changes);
                    while let Some(change) =
                        std::future::poll_fn(|cx| changes.as_mut().poll_next(cx)).await
                    {
                        let Ok(args) = change.args() else { continue };
                        if args.new_owner().is_some() {
                            let registered = async {
                                zbus::Proxy::new(
                                    &connection,
                                    WATCHER,
                                    "/StatusNotifierWatcher",
                                    WATCHER,
                                )
                                .await?
                                .call::<_, _, ()>("RegisterStatusNotifierItem", &(name.as_str(),))
                                .await
                            }
                            .await;
                            log::info(&format!(
                                "tray watcher appeared; registered: {}",
                                registered.is_ok()
                            ));
                        }
                    }
                })
            })
            .ok();
    }

    /// Shows a new model; emits only what changed.
    pub fn update(&self, model: TrayModel) {
        let (icon, tooltip, menu, status) = {
            let mut shared = lock(&self.state);
            if shared.model == model {
                return;
            }
            let icon = shared.model.state != model.state;
            let tooltip = icon
                || shared.model.description != model.description
                || shared.model.detail != model.detail;
            let menu = shared.model.menu != model.menu;
            shared.model = model;
            if menu {
                shared.revision += 1;
            }
            let status = if shared.model.state == TrayState::Attention {
                "NeedsAttention"
            } else {
                "Active"
            };
            (icon, tooltip, menu.then_some(shared.revision), status)
        };
        let server = self.connection.object_server();
        zbus::block_on(async {
            if let Ok(item) = server.interface::<_, Item>(ITEM_PATH).await {
                let emitter = item.signal_emitter();
                if icon {
                    let _ = Item::new_icon(emitter).await;
                    let _ = Item::new_status(emitter, status).await;
                }
                if tooltip {
                    let _ = Item::new_tool_tip(emitter).await;
                }
            }
            if let Some(revision) = menu
                && let Ok(menu) = server.interface::<_, Menu>(MENU_PATH).await
            {
                let _ = Menu::layout_updated(menu.signal_emitter(), revision, 0).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_numbers_items_from_one() {
        let menu = vec![
            MenuItem {
                label: "Pause_Sharing".into(),
                enabled: true,
                action: Some(TrayAction::ToggleSharing),
            },
            MenuItem::separator(),
        ];
        let (root, props, children) = layout(&menu);
        assert_eq!(root, 0);
        assert!(props.contains_key("children-display"));
        assert_eq!(children.len(), 2);
        let first = properties(&menu[0]);
        assert_eq!(
            <&str>::try_from(&*first["label"]).unwrap(),
            "Pause__Sharing",
            "underscores are not mnemonics"
        );
        assert!(properties(&menu[1]).contains_key("type"));
    }
}
