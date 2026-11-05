//! Send Files…: choose files or folders, then the computer they go to when
//! more than one can receive them, as the other two apps do.

use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use inputweave_app_core::proto::{SendFiles, client_message::Command};
use inputweave_app_core::text::keys;
use inputweave_app_core::{device_text, log, ui};

use crate::strings::{t, tr};
use crate::ui::Ui;

#[derive(Clone, Copy, Debug)]
pub enum What {
    Files,
    Folders,
}

fn paths(list: &gio::ListModel) -> Vec<String> {
    (0..list.n_items())
        .filter_map(|i| list.item(i).and_downcast::<gio::File>())
        .filter_map(|f| f.path())
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

pub fn choose_and_send(ui: &Rc<Ui>, what: What) {
    let window = ui.present_window();
    let parent = window.window().clone();
    let model = ui.snapshot();
    if model.file_receivers().is_empty() {
        let dialog = adw::AlertDialog::new(
            Some(&t(keys::SEND_TO_NONE_TITLE)),
            Some(&tr(&device_text::no_file_receiver(&model))),
        );
        dialog.add_response("ok", &t(keys::OK));
        dialog.present(Some(&parent));
        return;
    }
    let ui = ui.clone();
    gtk::glib::spawn_future_local(async move {
        let dialog = gtk::FileDialog::builder()
            .modal(true)
            .title(t(match what {
                What::Files => keys::SEND_FILES_CHOOSE_TITLE,
                What::Folders => keys::SEND_FILES_CHOOSE_FOLDERS,
            }))
            .build();
        let chosen = match what {
            What::Files => dialog.open_multiple_future(Some(&parent)).await,
            What::Folders => dialog.select_multiple_folders_future(Some(&parent)).await,
        };
        let Ok(chosen) = chosen else { return };
        let paths = paths(&chosen);
        if paths.is_empty() {
            return;
        }
        let receivers: Vec<(String, String)> = ui
            .snapshot()
            .file_receivers()
            .iter()
            .map(|d| (d.id.clone(), d.name.clone()))
            .collect();
        match receivers.as_slice() {
            [] => {}
            [(id, name)] => send(&ui, id.clone(), name.clone(), paths),
            _ => {
                let dialog = adw::AlertDialog::new(
                    Some(&t(keys::SEND_FILES_TITLE)),
                    Some(&t(keys::SEND_FILES_TO)),
                );
                dialog.add_response("cancel", &t(keys::CANCEL));
                for (id, name) in &receivers {
                    dialog.add_response(id, name);
                }
                dialog.set_close_response("cancel");
                let response = dialog.choose_future(Some(&parent)).await;
                if let Some((id, name)) = receivers.iter().find(|(id, _)| *id == response.as_str())
                {
                    send(&ui, id.clone(), name.clone(), paths);
                }
            }
        }
    });
}

pub(crate) fn send(ui: &Rc<Ui>, device_id: String, name: String, paths: Vec<String>) {
    log::info(&format!("sending {} items to {device_id}", paths.len()));
    let window = ui.window().widget();
    ui.command_then(
        Command::SendFiles(SendFiles { device_id, paths }),
        move |result| {
            if result.is_err() {
                let dialog = adw::AlertDialog::new(
                    Some(&tr(&ui!(keys::SEND_FILES_FAILED, name.clone()))),
                    Some(&tr(&ui!(keys::SEND_FILES_CHECK_CONNECTED, name.clone()))),
                );
                dialog.add_response("ok", &t(keys::OK));
                dialog.present(Some(&window));
            }
            true
        },
    );
}
