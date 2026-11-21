//! Sync (#42) in the GTK app, from the same session as Qt's
//! (`matters_sync_app::SyncApp`).
//!
//! A button at the foot of the sidebar states where sync stands, and opens
//! the Sync window. That window:
//! - while not syncing, offers three ways in: start, join with an
//!   invitation, recover from the recovery kit;
//! - shows the operator's command while the relay doesn't know the
//!   collection;
//! - lists the devices, with Remove for the admin;
//! - adds a device with an invitation;
//! - stops syncing, asking first when changes are not yet on the relay.
//!
//! The recovery kit is shown once, until kept. The session tells this
//! module of changes from its own threads by setting a flag, which the main
//! loop polls.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gtk4::prelude::*;
use gtk4::{glib, Align, Box as GtkBox, Button, Entry, Label, ListBox, Orientation, TextView};
use matters_sync_app::{Health, LeaveRefused, Phase, SyncApp, View};

/// What the sidebar and the window say, in plain words. A change saved
/// here and not on the relay is never called backed up.
pub(crate) fn status_text(available: bool, v: &View) -> String {
    if !available {
        return "Sync is unavailable on this device".into();
    }
    match v.phase {
        Phase::Off => return "Not syncing: everything is on this device only".into(),
        Phase::Joining => return "Waiting for the other device to add this one".into(),
        Phase::Recovering => return "Recovering from the recovery kit".into(),
        Phase::On => {}
    }
    match &v.health {
        Health::UpToDate => "Synced".into(),
        Health::Syncing => "Syncing".into(),
        Health::Idle => "Sync is starting".into(),
        Health::Retrying(_) => {
            "Can't reach the relay. Changes are saved on this device and will be sent later.".into()
        }
        Health::WaitingForRelay => "Waiting for the relay to be set up for this collection".into(),
        Health::Removed => "This device was removed from sync. Its tasks stay here.".into(),
        Health::StorageFull => {
            "The relay's storage is full. Changes are saved on this device only.".into()
        }
        Health::Conflict => {
            "The relay refused a change. Changes are saved on this device only.".into()
        }
        Health::Unsupported(_) => "The relay doesn't work with this version of Matters.".into(),
    }
}

pub(crate) fn unsent_text(n: usize) -> String {
    if n == 1 {
        "1 change is saved on this device and not yet on the relay.".into()
    } else {
        format!("{n} changes are saved on this device and not yet on the relay.")
    }
}

fn text_of(view: &TextView) -> String {
    let buffer = view.buffer();
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .to_string()
}

fn heading(text: &str) -> Label {
    Label::builder()
        .label(text)
        .halign(Align::Start)
        .css_classes(["heading"])
        .build()
}

fn wrapped(text: &str) -> Label {
    Label::builder()
        .label(text)
        .halign(Align::Start)
        .wrap(true)
        .xalign(0.0)
        .build()
}

fn selectable(text: &str) -> Label {
    let label = wrapped(text);
    label.set_selectable(true);
    label.set_wrap_mode(gtk4::pango::WrapMode::Char);
    label
}

/// The Sync window and the sidebar's sync button.
pub(crate) struct SyncPanel {
    pub(crate) app: Option<SyncApp>,
    pub(crate) button: Button,
    pub(crate) window: libadwaita::Window,
    pub(crate) status: Label,
    pub(crate) unsent: Label,
    pub(crate) error: Label,
    pub(crate) off_box: GtkBox,
    pub(crate) endpoint: Entry,
    pub(crate) create: Button,
    pub(crate) invitation_field: TextView,
    pub(crate) join: Button,
    pub(crate) collection_field: Entry,
    pub(crate) words_field: TextView,
    pub(crate) recover: Button,
    pub(crate) provision: Label,
    pub(crate) devices: ListBox,
    pub(crate) invite: Button,
    pub(crate) invitation: Label,
    pub(crate) cancel_invitation: Button,
    pub(crate) leave: Button,
    pub(crate) leave_box: GtkBox,
    pub(crate) leave_text: Label,
    pub(crate) leave_keep: Button,
    pub(crate) leave_confirm: Button,
    pub(crate) kit_window: libadwaita::Window,
    pub(crate) kit_text: Label,
    pub(crate) kit_kept: Button,
    shown: RefCell<View>,
}

impl SyncPanel {
    pub(crate) fn build(app: Option<SyncApp>, parent: &impl IsA<gtk4::Window>) -> Rc<SyncPanel> {
        let button = Button::builder()
            .icon_name("network-transmit-receive-symbolic")
            .css_classes(["flat"])
            .build();
        let content = GtkBox::new(Orientation::Vertical, 8);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        let status = wrapped("");
        let unsent = wrapped("");
        unsent.add_css_class("dim-label");
        let error = wrapped("");
        error.add_css_class("error");
        content.append(&status);
        content.append(&unsent);
        content.append(&error);
        if app.is_none() {
            content.append(&wrapped(
                "Sync keeps this device's keys in the desktop's keyring, and none is running. \
                 Matters works on this device without it.",
            ));
        }

        let off_box = GtkBox::new(Orientation::Vertical, 6);
        off_box.append(&wrapped(
            "Your tasks are encrypted on this device before they leave it. The relay stores \
             them but can't read them; it can see how much you store and when you sync.",
        ));
        off_box.append(&heading("Relay address"));
        let endpoint = Entry::builder()
            .placeholder_text("https://relay.example.org")
            .input_purpose(gtk4::InputPurpose::Url)
            .build();
        endpoint.update_property(&[gtk4::accessible::Property::Label("Relay address")]);
        off_box.append(&endpoint);
        let create = Button::with_label("Start syncing from this device");
        off_box.append(&create);
        off_box.append(&heading("Join from another device"));
        let invitation_field = TextView::builder()
            .wrap_mode(gtk4::WrapMode::Char)
            .height_request(60)
            .build();
        invitation_field.update_property(&[gtk4::accessible::Property::Label(
            "Invitation from the other device",
        )]);
        off_box.append(&invitation_field);
        let join = Button::with_label("Join");
        off_box.append(&join);
        off_box.append(&heading("Recover after losing every device"));
        let collection_field = Entry::builder()
            .placeholder_text("Collection ID from the recovery kit")
            .build();
        collection_field.update_property(&[gtk4::accessible::Property::Label("Collection ID")]);
        off_box.append(&collection_field);
        let words_field = TextView::builder()
            .wrap_mode(gtk4::WrapMode::Word)
            .height_request(60)
            .build();
        words_field.update_property(&[gtk4::accessible::Property::Label("Recovery words")]);
        off_box.append(&words_field);
        let recover = Button::with_label("Recover");
        off_box.append(&recover);
        content.append(&off_box);

        let provision = selectable("");
        content.append(&provision);
        let devices = ListBox::new();
        devices.add_css_class("boxed-list");
        content.append(&devices);
        let invite = Button::with_label("Add a device");
        content.append(&invite);
        let invitation = selectable("");
        content.append(&invitation);
        let cancel_invitation = Button::with_label("Cancel");
        content.append(&cancel_invitation);
        let leave = Button::with_label("Stop syncing on this device");
        content.append(&leave);
        // Stopping asks first, with the safe choice first.
        let leave_box = GtkBox::new(Orientation::Vertical, 6);
        let leave_text = wrapped("");
        leave_box.append(&leave_text);
        let row = GtkBox::new(Orientation::Horizontal, 6);
        row.set_halign(Align::End);
        let leave_keep = Button::with_label("Keep syncing");
        let leave_confirm = Button::with_label("Stop syncing");
        leave_confirm.add_css_class("destructive-action");
        row.append(&leave_keep);
        row.append(&leave_confirm);
        leave_box.append(&row);
        leave_box.set_visible(false);
        content.append(&leave_box);

        let toolbar = libadwaita::ToolbarView::new();
        toolbar.add_top_bar(&libadwaita::HeaderBar::new());
        toolbar.set_content(Some(
            &gtk4::ScrolledWindow::builder()
                .child(&content)
                .min_content_height(420)
                .build(),
        ));
        let window = libadwaita::Window::builder()
            .title("Sync")
            .modal(true)
            .transient_for(parent)
            .hide_on_close(true)
            .default_width(480)
            .content(&toolbar)
            .build();

        // The recovery kit, once.
        let kit_box = GtkBox::new(Orientation::Vertical, 8);
        kit_box.set_margin_top(12);
        kit_box.set_margin_bottom(12);
        kit_box.set_margin_start(12);
        kit_box.set_margin_end(12);
        kit_box.append(&wrapped(
            "Write these down and keep them somewhere safe. If every device is lost, they are \
             the only way to get your tasks back. Nobody else holds them: not the relay's \
             operator, and not Matters.",
        ));
        let kit_text = selectable("");
        kit_box.append(&kit_text);
        let kit_kept = Button::with_label("I've written them down");
        kit_kept.set_halign(Align::End);
        kit_kept.add_css_class("suggested-action");
        kit_box.append(&kit_kept);
        let kit_toolbar = libadwaita::ToolbarView::new();
        kit_toolbar.add_top_bar(
            &libadwaita::HeaderBar::builder()
                .show_end_title_buttons(false)
                .build(),
        );
        kit_toolbar.set_content(Some(&kit_box));
        let kit_window = libadwaita::Window::builder()
            .title("Your recovery kit")
            .modal(true)
            .transient_for(parent)
            .deletable(false)
            .default_width(480)
            .content(&kit_toolbar)
            .build();

        let panel = Rc::new(SyncPanel {
            app,
            button,
            window,
            status,
            unsent,
            error,
            off_box,
            endpoint,
            create,
            invitation_field,
            join,
            collection_field,
            words_field,
            recover,
            provision,
            devices,
            invite,
            invitation,
            cancel_invitation,
            leave,
            leave_box,
            leave_text,
            leave_keep,
            leave_confirm,
            kit_window,
            kit_text,
            kit_kept,
            shown: RefCell::new(View::default()),
        });
        panel.connect();
        panel.refresh();
        panel
    }

    /// The flag the session sets from its threads; the main loop polls it.
    pub(crate) fn notice() -> (Arc<AtomicBool>, impl Fn() + Send + Sync + 'static) {
        let dirty = Arc::new(AtomicBool::new(true));
        let flag = dirty.clone();
        (dirty, move || flag.store(true, Ordering::SeqCst))
    }

    /// Refresh whenever the session says something changed. `reload` runs
    /// after a round finished, so the lists show what arrived.
    pub(crate) fn follow(self: &Rc<Self>, dirty: Arc<AtomicBool>, reload: impl Fn() + 'static) {
        let panel = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(250), move || {
            let Some(panel) = panel.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if dirty.swap(false, Ordering::SeqCst) && panel.refresh() {
                reload();
            }
            glib::ControlFlow::Continue
        });
    }

    fn connect(self: &Rc<Self>) {
        let p = Rc::downgrade(self);
        self.button.connect_clicked(move |_| {
            if let Some(p) = p.upgrade() {
                // The focus is set on the window, which keeps it until it
                // is shown: a grab before the window is mapped is lost.
                if p.shown.borrow().phase == Phase::Off {
                    GtkWindowExt::set_focus(&p.window, Some(&p.endpoint));
                }
                p.window.present();
            }
        });
        let on = |button: &Button, f: fn(&SyncPanel, &SyncApp)| {
            let p = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(p) = p.upgrade() {
                    if let Some(app) = p.app.clone() {
                        f(&p, &app);
                        p.refresh();
                    }
                }
            });
        };
        on(&self.create, |p, app| {
            let _ = app.create(&p.endpoint.text());
        });
        on(&self.join, |p, app| {
            let _ = app.join(&p.endpoint.text(), &text_of(&p.invitation_field));
        });
        on(&self.recover, |p, app| {
            let _ = app.recover(
                &p.endpoint.text(),
                &p.collection_field.text(),
                &text_of(&p.words_field),
            );
        });
        on(&self.invite, |_, app| {
            let _ = app.invite();
        });
        on(&self.cancel_invitation, |_, app| app.cancel_invitation());
        on(&self.kit_kept, |_, app| app.kit_kept());
        let p = Rc::downgrade(self);
        self.leave.connect_clicked(move |_| {
            if let Some(p) = p.upgrade() {
                let unsent = p.shown.borrow().unsent;
                p.leave_text.set_label(&if unsent > 0 {
                    format!(
                        "{} Stopping now keeps them on this device only: your other devices \
                         will never get them.",
                        unsent_text(unsent)
                    )
                } else {
                    "Every task stays on this device. Your other devices keep syncing without it."
                        .into()
                });
                p.leave_confirm.set_label(if unsent > 0 {
                    "Stop and keep changes here only"
                } else {
                    "Stop syncing"
                });
                p.leave_box.set_visible(true);
                GtkWindowExt::set_focus(&p.window, Some(&p.leave_keep));
            }
        });
        let p = Rc::downgrade(self);
        self.leave_keep.connect_clicked(move |_| {
            if let Some(p) = p.upgrade() {
                p.leave_box.set_visible(false);
            }
        });
        let p = Rc::downgrade(self);
        self.leave_confirm.connect_clicked(move |_| {
            if let Some(p) = p.upgrade() {
                if let Some(app) = p.app.clone() {
                    let unsent = p.shown.borrow().unsent;
                    match app.leave(unsent > 0) {
                        Ok(()) | Err(LeaveRefused::Failed(_)) => {}
                        // More arrived since: ask again with the new count.
                        Err(LeaveRefused::Unsent(_)) => {
                            p.refresh();
                            p.leave.emit_clicked();
                            return;
                        }
                    }
                }
                p.leave_box.set_visible(false);
                p.refresh();
            }
        });
    }

    /// Show the session's view. True when a round just finished.
    pub(crate) fn refresh(self: &Rc<Self>) -> bool {
        let available = self.app.is_some();
        let v = self.app.as_ref().map(SyncApp::view).unwrap_or_default();
        let status = status_text(available, &v);
        self.button.set_tooltip_text(Some(&status));
        self.button
            .update_property(&[gtk4::accessible::Property::Label(&format!(
                "Sync: {status}"
            ))]);
        let attention = v.phase == Phase::On
            && !matches!(v.health, Health::UpToDate | Health::Syncing | Health::Idle);
        if attention {
            self.button.add_css_class("error");
        } else {
            self.button.remove_css_class("error");
        }
        self.status.set_label(&status);
        let on = v.phase == Phase::On;
        self.unsent.set_label(&unsent_text(v.unsent));
        self.unsent.set_visible(on && v.unsent > 0);
        self.error.set_label(v.error.as_deref().unwrap_or(""));
        self.error.set_visible(v.error.is_some());
        self.off_box.set_visible(available && v.phase == Phase::Off);
        if self.endpoint.text().is_empty() {
            if let Some(e) = &v.endpoint {
                self.endpoint.set_text(e);
            }
        }
        let provision = match (&v.provisioning, &v.health) {
            (Some(p), Health::WaitingForRelay) => format!(
                "The relay's operator sets it up for this collection with:\n{}",
                p.command
            ),
            _ => String::new(),
        };
        self.provision.set_label(&provision);
        self.provision.set_visible(!provision.is_empty());
        while let Some(row) = self.devices.first_child() {
            self.devices.remove(&row);
        }
        for d in &v.devices {
            let row = GtkBox::new(Orientation::Horizontal, 6);
            row.set_margin_start(6);
            row.set_margin_end(6);
            let name = match (d.this_device, d.admin) {
                (true, true) => format!("{} (this device, admin)", d.short),
                (true, false) => format!("{} (this device)", d.short),
                (false, true) => format!("{} (admin)", d.short),
                (false, false) => d.short.clone(),
            };
            row.append(
                &Label::builder()
                    .label(&name)
                    .hexpand(true)
                    .halign(Align::Start)
                    .build(),
            );
            if v.admin && !d.this_device {
                let remove = Button::with_label("Remove");
                remove.update_property(&[gtk4::accessible::Property::Label(&format!(
                    "Remove device {}",
                    d.short
                ))]);
                let (app, id) = (self.app.clone(), d.id);
                let p = Rc::downgrade(self);
                remove.connect_clicked(move |_| {
                    if let Some(app) = &app {
                        let _ = app.remove_device(&id);
                    }
                    if let Some(p) = p.upgrade() {
                        p.refresh();
                    }
                });
                row.append(&remove);
            }
            self.devices.append(&row);
        }
        self.devices.set_visible(on && !v.devices.is_empty());
        self.invite
            .set_visible(on && v.admin && v.invitation.is_none());
        let invitation = v.invitation.as_ref().map_or(String::new(), |i| {
            format!(
                "On the new device, open Sync, choose Join and paste this invitation. It works \
                 once, within an hour. Anyone who sees it can ask to join, so share it only with \
                 your own device.\n\n{i}"
            )
        });
        self.invitation.set_label(&invitation);
        self.invitation.set_visible(v.invitation.is_some());
        self.cancel_invitation.set_visible(v.invitation.is_some());
        self.leave.set_visible(v.phase != Phase::Off);
        match &v.kit {
            Some(kit) => {
                self.kit_text.set_label(&format!(
                    "Relay address\n{}\n\nCollection ID\n{}\n\nRecovery words\n{}",
                    kit.endpoint, kit.collection, kit.words
                ));
                if !self.kit_window.is_visible() {
                    GtkWindowExt::set_focus(&self.kit_window, Some(&self.kit_kept));
                    self.kit_window.present();
                }
            }
            None => self.kit_window.set_visible(false),
        }
        let arrived =
            v.health == Health::UpToDate && self.shown.borrow().health != Health::UpToDate;
        *self.shown.borrow_mut() = v;
        arrived
    }
}
