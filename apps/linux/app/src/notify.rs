//! Desktop notifications through GApplication (`GNotification`): files
//! received, files waiting for an answer, and the service stopping. Their
//! buttons are app actions, so they work while the app runs in the
//! background. When to notify is `inputweave_app_core::news`'s and
//! `service`'s to decide.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use inputweave_app_core::news::TransferUpdate;
use inputweave_app_core::proto::{AnswerFileOffer, client_message::Command};
use inputweave_app_core::service::ServiceAction;
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;
use inputweave_app_core::{paths, size, transfer_text, ui};

use crate::strings::{t, tr};
use crate::ui::Ui;
use crate::window::Page;

const SERVICE_STOPPED: &str = "service-stopped";

fn offer_id(transfer_id: &str) -> String {
    format!("offer-{transfer_id}")
}

fn string_action(
    ui: &Rc<Ui>,
    name: &str,
    run: impl Fn(&Rc<Ui>, String) + 'static,
) -> gio::SimpleAction {
    let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
    let ui = Rc::downgrade(ui);
    action.connect_activate(move |_, parameter| {
        if let (Some(ui), Some(value)) = (ui.upgrade(), parameter.and_then(|p| p.get::<String>())) {
            run(&ui, value);
        }
    });
    action
}

pub fn install_actions(ui: &Rc<Ui>) {
    let app = ui.app.clone();
    app.add_action(&string_action(ui, "open-folder", |ui, path| {
        let file = gio::File::for_path(path);
        let launcher = gtk::FileLauncher::new(Some(&file));
        let window = ui.window().window().clone();
        launcher.launch(Some(&window), gio::Cancellable::NONE, |_| {});
    }));
    app.add_action(&string_action(ui, "accept-offer", |ui, id| {
        answer(ui, id, true)
    }));
    app.add_action(&string_action(ui, "decline-offer", |ui, id| {
        answer(ui, id, false)
    }));
    app.add_action(&string_action(ui, "show-page", |ui, page| {
        ui.show_window(Page::from_name(&page));
    }));
    let start = gio::SimpleAction::new("start-service", None);
    let weak = Rc::downgrade(ui);
    start.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.control_service(ServiceAction::Start);
        }
    });
    app.add_action(&start);
}

fn answer(ui: &Rc<Ui>, transfer_id: String, accept: bool) {
    ui.app.withdraw_notification(&offer_id(&transfer_id));
    ui.command(Command::AnswerFileOffer(AnswerFileOffer {
        transfer_id,
        accept,
    }));
}

pub fn transfers(ui: &Rc<Ui>, model: &Snapshot, update: &TransferUpdate) {
    for id in &update.withdraw {
        ui.app.withdraw_notification(&offer_id(id));
    }
    for offer in &update.ask {
        let device = model.name_of(&offer.device_id);
        let notification = gio::Notification::new(&tr(&ui!(keys::NOTIFY_OFFER_TITLE, device)));
        notification.set_body(Some(&tr(&ui!(
            keys::NOTIFY_OFFER_BODY,
            offer.name.clone(),
            size::format(offer.size)
        ))));
        notification.set_priority(gio::NotificationPriority::Urgent);
        let id = offer.transfer_id.to_variant();
        notification.add_button_with_target_value(
            &t(keys::TRANSFER_ACCEPT),
            "app.accept-offer",
            Some(&id),
        );
        notification.add_button_with_target_value(
            &t(keys::TRANSFER_DECLINE),
            "app.decline-offer",
            Some(&id),
        );
        notification.set_default_action_and_target_value(
            "app.show-page",
            Some(&Page::Files.name().to_variant()),
        );
        ui.app
            .send_notification(Some(&offer_id(&offer.transfer_id)), &notification);
    }
    for received in &update.announce {
        let device = model.name_of(&received.device_id);
        let Some(text) = transfer_text::received(&received.files, &device) else {
            continue;
        };
        let notification = gio::Notification::new(&tr(&text));
        if let Some(folder) = received_folder(model, &received.files) {
            notification.add_button_with_target_value(
                &t(keys::NOTIFY_OPEN_FOLDER),
                "app.open-folder",
                Some(&folder.to_variant()),
            );
        }
        notification.set_default_action_and_target_value(
            "app.show-page",
            Some(&Page::Files.name().to_variant()),
        );
        ui.app.send_notification(
            Some(&format!(
                "received-{}-{}",
                received.device_id, received.files[0].transfer_id
            )),
            &notification,
        );
    }
}

/// The folder received files are in: the receive folder, or the one the
/// first file went to.
fn received_folder(
    model: &Snapshot,
    files: &[inputweave_app_core::proto::FileTransfer],
) -> Option<String> {
    if !model.transfers.receive_folder.is_empty() {
        return Some(model.transfers.receive_folder.clone());
    }
    files
        .iter()
        .find(|f| !f.path.is_empty())
        .and_then(|f| paths::containing_folder(std::path::Path::new(&f.path)))
        .map(|p| p.to_string_lossy().into_owned())
}

const FAILURE: &str = "failure";

/// How long a failure stays in the notification list.
const FAILURE_SHOWN: std::time::Duration = std::time::Duration::from_secs(10);

/// Something the user did from the tray menu or a notification did not
/// work: the latest such, briefly, as there is no window to show it in.
pub fn failure(ui: &Rc<Ui>, text: &str) {
    transient(ui, FAILURE, text, FAILURE_SHOWN);
}

thread_local! {
    /// The timer that takes back each transient notification, by its id:
    /// a new note under the same id cancels the last one's, so it is not
    /// taken back early.
    static WITHDRAW: std::cell::RefCell<std::collections::HashMap<&'static str, glib::SourceId>> =
        std::cell::RefCell::default();
}

/// A notification of its own under `id`, replacing the last one there and
/// taken back after `shown`.
fn transient(ui: &Rc<Ui>, id: &'static str, text: &str, shown: std::time::Duration) {
    let notification = gio::Notification::new("InputWeave");
    notification.set_body(Some(text));
    ui.app.send_notification(Some(id), &notification);
    let app = ui.app.downgrade();
    let source = glib::timeout_add_local_once(shown, move || {
        WITHDRAW.with(|w| w.borrow_mut().remove(id));
        if let Some(app) = app.upgrade() {
            app.withdraw_notification(id);
        }
    });
    if let Some(previous) = WITHDRAW.with(|w| w.borrow_mut().insert(id, source)) {
        previous.remove();
    }
}

const HUD: &str = "hud";
const HUD_SHOWN: std::time::Duration = std::time::Duration::from_secs(6);

/// The HUD where nothing can be put over the screen (GNOME on Wayland): a
/// notification of its own, replaced by the next and taken back soon.
pub fn hud(ui: &Rc<Ui>, text: &str) {
    transient(ui, HUD, text, HUD_SHOWN);
}

pub fn service_stopped(ui: &Rc<Ui>) {
    let notification = gio::Notification::new(&t(keys::NOTIFY_SERVICE_STOPPED));
    notification.add_button(&t(keys::ACTION_START_SERVICE), "app.start-service");
    notification.set_default_action_and_target_value(
        "app.show-page",
        Some(&Page::Troubleshooting.name().to_variant()),
    );
    ui.app
        .send_notification(Some(SERVICE_STOPPED), &notification);
}

pub fn service_running(ui: &Rc<Ui>) {
    ui.app.withdraw_notification(SERVICE_STOPPED);
}
