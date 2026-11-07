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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Open,
    SendFiles,
    ToggleSharing,
    /// Takes this computer offline for now, or brings it back.
    ToggleOffline,
    /// Uses this profile whatever the network; an empty id goes back to
    /// Automatic.
    SelectProfile(String),
    /// Opens the Profiles page.
    ManageProfiles,
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
    /// Whether it shows a check mark, and whether it is checked; `None`
    /// for an item with no check mark.
    pub checked: Option<bool>,
    /// The items of the submenu it opens; empty for an item that opens
    /// none.
    pub children: Vec<MenuItem>,
}

impl MenuItem {
    pub fn new(label: String, enabled: bool, action: TrayAction) -> MenuItem {
        MenuItem {
            label,
            enabled,
            action: Some(action),
            checked: None,
            children: Vec::new(),
        }
    }

    /// An item that opens a submenu of `children`.
    pub fn submenu(label: String, children: Vec<MenuItem>) -> MenuItem {
        MenuItem {
            label,
            enabled: true,
            action: None,
            checked: None,
            children,
        }
    }

    pub fn separator() -> MenuItem {
        MenuItem {
            label: String::new(),
            enabled: true,
            action: None,
            checked: None,
            children: Vec::new(),
        }
    }

    fn is_separator(&self) -> bool {
        self.action.is_none() && self.label.is_empty() && self.children.is_empty()
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
        owned(Variant::from(
            item.enabled && (item.action.is_some() || !item.children.is_empty()),
        )),
    );
    if !item.children.is_empty() {
        props.insert("children-display".into(), owned(Variant::from("submenu")));
    }
    if let Some(checked) = item.checked {
        props.insert("toggle-type".into(), owned(Variant::from("checkmark")));
        props.insert(
            "toggle-state".into(),
            owned(Variant::from(i32::from(checked))),
        );
    }
    props
}

/// Menu item ids: a top-level item's is its place plus one (0 is the
/// root), so it stays the same whatever a submenu holds; an item of a
/// submenu's is its parent's times this, plus its place plus one.
const SUBMENU_IDS: i32 = 1000;

/// The item with id `id`, at the top level or in a submenu.
fn find(menu: &[MenuItem], id: i32) -> Option<&MenuItem> {
    if id <= 0 {
        return None;
    }
    if id < SUBMENU_IDS {
        return menu.get((id - 1) as usize);
    }
    let parent = find(menu, id / SUBMENU_IDS)?;
    parent
        .children
        .get(((id % SUBMENU_IDS) as usize).wrapping_sub(1))
}

/// Every item's id, submenus' included.
fn ids(menu: &[MenuItem]) -> Vec<i32> {
    let mut all = Vec::new();
    for (i, item) in menu.iter().enumerate() {
        let id = i as i32 + 1;
        all.push(id);
        all.extend((0..item.children.len()).map(|j| id * SUBMENU_IDS + j as i32 + 1));
    }
    all
}

/// The layout of `item`, numbered `id`, with its submenu's.
fn layout_of(item: &MenuItem, id: i32) -> Layout {
    let children = item
        .children
        .iter()
        .enumerate()
        .map(|(j, child)| {
            owned(Variant::from(layout_of(
                child,
                id * SUBMENU_IDS + j as i32 + 1,
            )))
        })
        .collect();
    (id, properties(item), children)
}

fn layout(menu: &[MenuItem]) -> Layout {
    let children = menu
        .iter()
        .enumerate()
        .map(|(i, item)| owned(Variant::from(layout_of(item, i as i32 + 1))))
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
        match find(&shared.model.menu, parent_id) {
            Some(item) => Ok((shared.revision, layout_of(item, parent_id))),
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
        let ids = if ids.is_empty() { self::ids(menu) } else { ids };
        ids.into_iter()
            .filter_map(|id| find(menu, id).map(|item| (id, properties(item))))
            .collect()
    }

    fn get_property(&self, id: i32, name: String) -> fdo::Result<OwnedVariant> {
        let shared = lock(&self.state);
        find(&shared.model.menu, id)
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
        }) = find(&shared.model.menu, id)
        {
            let _ = shared.actions.try_send(action.clone());
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
            MenuItem::new("Pause_Sharing".into(), true, TrayAction::ToggleSharing),
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

    #[test]
    fn submenus_number_their_items_apart_and_check_them() {
        let choice = |label: &str, id: &str, checked: bool| MenuItem {
            checked: Some(checked),
            ..MenuItem::new(label.into(), true, TrayAction::SelectProfile(id.into()))
        };
        let menu = vec![
            MenuItem::separator(),
            MenuItem::submenu(
                "Profile".into(),
                vec![
                    choice("Automatic", "", true),
                    choice("Office", "office", false),
                    MenuItem::separator(),
                    MenuItem::new("Manage Profiles…".into(), true, TrayAction::ManageProfiles),
                ],
            ),
            MenuItem::new("Quit".into(), true, TrayAction::Quit),
        ];
        // The top level keeps its numbers whatever the submenu holds.
        assert_eq!(
            find(&menu, 3).and_then(|i| i.action.clone()),
            Some(TrayAction::Quit)
        );
        assert_eq!(
            find(&menu, 2002).and_then(|i| i.action.clone()),
            Some(TrayAction::SelectProfile("office".into()))
        );
        assert_eq!(
            find(&menu, 2004).and_then(|i| i.action.clone()),
            Some(TrayAction::ManageProfiles)
        );
        assert!(find(&menu, 2005).is_none() && find(&menu, 2000).is_none());
        assert!(find(&menu, 1001).is_none() && find(&menu, 0).is_none());
        assert_eq!(ids(&menu), [1, 2, 2001, 2002, 2003, 2004, 3]);
        let (_, _, top) = layout(&menu);
        assert_eq!(top.len(), 3);
        let parent = properties(&menu[1]);
        assert_eq!(
            <&str>::try_from(&*parent["children-display"]).unwrap(),
            "submenu"
        );
        assert!(<bool>::try_from(&*parent["enabled"]).unwrap());
        let (id, _, children) = layout_of(&menu[1], 2);
        assert_eq!(id, 2);
        assert_eq!(children.len(), 4);
        let automatic = properties(&menu[1].children[0]);
        assert_eq!(
            <&str>::try_from(&*automatic["toggle-type"]).unwrap(),
            "checkmark"
        );
        assert_eq!(<i32>::try_from(&*automatic["toggle-state"]).unwrap(), 1);
        let office = properties(&menu[1].children[1]);
        assert_eq!(<i32>::try_from(&*office["toggle-state"]).unwrap(), 0);
        assert!(!properties(&menu[2]).contains_key("toggle-type"));
    }
}
