//! About InputWeave: the icon, the name and version, then
//! the license where it can be seen at once ("Trial" with Buy a License…,
//! or the woven "Licensed" label), kept up to date while it is open, and
//! the GPL. AdwAboutDialog would keep the license behind its Details page,
//! so this is a dialog of its own in the same shape.

use std::rc::Rc;

use adw::prelude::*;
use inputweave_app_core::polar;
use inputweave_app_core::text::keys;

use crate::license;
use crate::strings::t;
use crate::ui::Ui;
use crate::woven::{Size, Woven};

pub struct About {
    pub dialog: adw::Dialog,
    /// "Trial" with Buy a License….
    trial: gtk::Box,
    woven: Woven,
}

impl About {
    pub fn new(ui: &Rc<Ui>, licensed: bool) -> Rc<About> {
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .halign(gtk::Align::Center)
            .margin_top(12)
            .margin_bottom(30)
            .margin_start(30)
            .margin_end(30)
            .build();
        content.append(
            &gtk::Image::builder()
                .icon_name(inputweave_app_core::APP_ID)
                .pixel_size(96)
                .css_classes(["icon-dropshadow"])
                .build(),
        );
        content.append(
            &gtk::Label::builder()
                .label("InputWeave")
                .css_classes(["title-1"])
                .margin_top(6)
                .build(),
        );
        content.append(
            &gtk::Label::builder()
                .label("ctsite.cc")
                .css_classes(["dim-label"])
                .build(),
        );
        content.append(
            &gtk::Label::builder()
                .label(env!("CARGO_PKG_VERSION"))
                .css_classes(["iw-version", "iw-numeric"])
                .halign(gtk::Align::Center)
                .margin_top(4)
                .build(),
        );

        let license_line = gtk::Box::builder()
            .spacing(10)
            .halign(gtk::Align::Center)
            .margin_top(10)
            .build();
        let woven = Woven::new(Size::Small, &t(keys::LICENSE_LICENSED));
        license_line.append(woven.widget());
        let trial = gtk::Box::builder().spacing(10).build();
        trial.append(
            &gtk::Label::builder()
                .label(t(keys::LICENSE_TRIAL))
                .css_classes(["iw-trial-pill"])
                .valign(gtk::Align::Center)
                .build(),
        );
        let buy = gtk::Button::builder()
            .label(t(keys::LICENSE_BUY))
            .css_classes(["link"])
            .build();
        let weak = Rc::downgrade(ui);
        buy.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                license::open(&ui, polar::CHECKOUT_URL);
            }
        });
        trial.append(&buy);
        license_line.append(&trial);
        content.append(&license_line);
        content.append(
            &gtk::Label::builder()
                .label(t(keys::ABOUT_GPL))
                .wrap(true)
                .justify(gtk::Justification::Center)
                .max_width_chars(44)
                .css_classes(["dim-label", "caption"])
                .margin_top(8)
                .build(),
        );
        content.append(
            &gtk::Label::builder()
                .label("© 2026 InputWeave contributors")
                .css_classes(["dim-label", "caption"])
                .build(),
        );

        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::builder().show_title(false).build());
        view.set_content(Some(&content));
        let dialog = adw::Dialog::builder()
            .title(t(keys::MENU_ABOUT))
            .content_width(360)
            .child(&view)
            .build();
        let about = Rc::new(About {
            dialog,
            trial,
            woven,
        });
        about.update(licensed);
        about
    }

    /// "Licensed", or "Trial" with Buy a License…, as the license is now.
    pub fn update(&self, licensed: bool) {
        self.trial.set_visible(!licensed);
        self.woven.widget().set_visible(licensed);
    }
}
