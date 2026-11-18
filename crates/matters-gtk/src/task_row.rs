//! One task in the list: an expander row with a done checkbox, schedule
//! badges, and (expanded) a notes editor and When buttons.
//!
//! Rows are created once per task and then updated in place (see
//! `Ui::refresh` and `matters_core::list_diff`), so expansion, focus and
//! in-progress notes survive list refreshes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk4::prelude::*;
use gtk4::{
    glib, Align, Box as GtkBox, Button, CheckButton, EventControllerFocus, Label, Orientation,
    ScrolledWindow, TextView, WrapMode,
};
use libadwaita::prelude::*;
use libadwaita::ExpanderRow;
use matters_core::{Schedule, Status, Task};

use crate::ui::Ui;

/// How long typing must pause before notes are saved.
const NOTES_SAVE_DELAY: Duration = Duration::from_millis(600);

/// The When buttons: label and `Schedule::parse` value.
pub(crate) const WHEN_BUTTONS: [(&str, &str); 3] = [
    ("Today", "today"),
    ("This Evening", "evening"),
    ("Someday", "someday"),
];

pub(crate) struct TaskRow {
    pub(crate) row: ExpanderRow,
    pub(crate) check: CheckButton,
    check_handler: glib::SignalHandlerId,
    evening: Label,
    date: Label,
    pub(crate) notes: TextView,
    /// True while `update` writes stored notes into the editor, so that
    /// write is not mistaken for typing.
    updating_notes: Rc<Cell<bool>>,
    #[cfg_attr(not(test), allow(dead_code))] // read by tests
    pub(crate) when_buttons: Vec<Button>,
}

impl TaskRow {
    pub(crate) fn new(ui: &Rc<Ui>, task: &Task) -> TaskRow {
        let row = ExpanderRow::builder().use_markup(false).build();

        let check = CheckButton::builder().valign(Align::Center).build();
        check.add_css_class("selection-mode");
        let check_handler = {
            let ui = Rc::downgrade(ui);
            let id = task.id.clone();
            check.connect_toggled(move |btn| {
                if let Some(ui) = ui.upgrade() {
                    ui.set_done(&id, btn.is_active());
                }
            })
        };
        row.add_prefix(&check);

        let evening = Label::builder()
            .label("🌙 Evening")
            .css_classes(["caption", "warning"])
            .valign(Align::Center)
            .build();
        let date = Label::builder()
            .css_classes(["caption", "dim-label"])
            .valign(Align::Center)
            .build();
        row.add_suffix(&evening);
        row.add_suffix(&date);

        // Expanded content: notes and When buttons.
        let detail = GtkBox::new(Orientation::Vertical, 8);
        detail.set_margin_top(8);
        detail.set_margin_bottom(8);
        detail.set_margin_start(12);
        detail.set_margin_end(12);

        let notes = TextView::builder()
            .wrap_mode(WrapMode::WordChar)
            .accepts_tab(false)
            .top_margin(6)
            .bottom_margin(6)
            .left_margin(6)
            .right_margin(6)
            .build();
        detail.append(
            &ScrolledWindow::builder()
                .child(&notes)
                .min_content_height(72)
                .css_classes(["card"])
                .build(),
        );

        let updating_notes = Rc::new(Cell::new(false));
        let pending_save: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
        let save_now = {
            let ui = Rc::downgrade(ui);
            let id = task.id.clone();
            let notes = notes.clone();
            let pending_save = pending_save.clone();
            Rc::new(move || {
                if let Some(source) = pending_save.borrow_mut().take() {
                    source.remove();
                }
                if let Some(ui) = ui.upgrade() {
                    let buffer = notes.buffer();
                    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
                    ui.save_notes(&id, text.as_str());
                }
            })
        };
        {
            let updating = updating_notes.clone();
            let pending_save = pending_save.clone();
            let save_now = save_now.clone();
            notes.buffer().connect_changed(move |_| {
                if updating.get() {
                    return;
                }
                if let Some(source) = pending_save.borrow_mut().take() {
                    source.remove();
                }
                let fired = pending_save.clone();
                let save_now = save_now.clone();
                let source = glib::timeout_add_local_once(NOTES_SAVE_DELAY, move || {
                    fired.borrow_mut().take(); // this source is finishing
                    save_now();
                });
                *pending_save.borrow_mut() = Some(source);
            });
        }
        {
            let focus = EventControllerFocus::new();
            let pending_save = pending_save.clone();
            focus.connect_leave(move |_| {
                if pending_save.borrow().is_some() {
                    save_now();
                }
            });
            notes.add_controller(focus);
        }

        let when_box = GtkBox::new(Orientation::Horizontal, 8);
        when_box.append(
            &Label::builder()
                .label("When:")
                .css_classes(["dim-label"])
                .build(),
        );
        let mut when_buttons = Vec::new();
        for (label, when) in WHEN_BUTTONS {
            let button = Button::builder().label(label).css_classes(["pill"]).build();
            let ui = Rc::downgrade(ui);
            let id = task.id.clone();
            button.connect_clicked(move |_| {
                if let (Some(ui), Some(schedule)) = (ui.upgrade(), Schedule::parse(when)) {
                    ui.schedule(&id, &schedule);
                }
            });
            when_box.append(&button);
            when_buttons.push(button);
        }
        detail.append(&when_box);
        row.add_row(&detail);

        let task_row = TaskRow {
            row,
            check,
            check_handler,
            evening,
            date,
            notes,
            updating_notes,
            when_buttons,
        };
        task_row.update(task);
        task_row
    }

    /// Show `task`'s current state without disturbing what the user is doing:
    /// expansion is kept, and notes being edited are not overwritten.
    pub(crate) fn update(&self, task: &Task) {
        self.row.set_title(&task.title);

        let done = task.status == Status::Completed;
        self.check.block_signal(&self.check_handler);
        self.check.set_active(done);
        self.check.unblock_signal(&self.check_handler);
        if done {
            self.row.add_css_class("dim-label");
        } else {
            self.row.remove_css_class("dim-label");
        }

        self.evening.set_visible(task.is_evening());
        match task.start_date {
            Some(date) => {
                self.date.set_label(&date.strftime("%b %d").to_string());
                self.date.set_visible(true);
            }
            None => self.date.set_visible(false),
        }

        let buffer = self.notes.buffer();
        let current = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
        if !self.notes.has_focus() && current.as_str() != task.notes {
            self.updating_notes.set(true);
            buffer.set_text(&task.notes);
            self.updating_notes.set(false);
        }
    }
}
