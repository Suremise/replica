//! The editor's schedule picker: how often (by hand, daily, on chosen days, at an interval, or
//! as typed), with the time, days or interval, written to the form's schedule as core's text
//! form ("daily 02:30", "weekly mon,thu 21:00", "every 6h").
//!
//! The form's text stays what's saved, and core parses it: the picker only writes it. Text set
//! some other way (a job opened, a test) is read back into the picker, and text it can't show
//! is "as typed", in a field of its own, so nothing is lost.

use chrono::{Timelike, Weekday};
use eframe::egui::{self, Response, Ui};
use replica_core::i18n::tr;
use replica_core::schedule::Schedule;

use crate::ui::theme::space;
use crate::ui::widgets::{dropdown_value, setting_row};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Manual,
    Daily,
    Weekly,
    Every,
    Custom,
}

const KINDS: [(Kind, &str); 5] = [
    (Kind::Manual, "editor-schedule-manual"),
    (Kind::Daily, "editor-schedule-daily"),
    (Kind::Weekly, "editor-schedule-weekly"),
    (Kind::Every, "editor-schedule-every"),
    (Kind::Custom, "editor-schedule-custom"),
];

const DAYS: [(Weekday, &str, &str); 7] = [
    (Weekday::Mon, "mon", "day-mon"),
    (Weekday::Tue, "tue", "day-tue"),
    (Weekday::Wed, "wed", "day-wed"),
    (Weekday::Thu, "thu", "day-thu"),
    (Weekday::Fri, "fri", "day-fri"),
    (Weekday::Sat, "sat", "day-sat"),
    (Weekday::Sun, "sun", "day-sun"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulePicker {
    pub kind: Kind,
    /// `HH:MM`, as typed.
    pub time: String,
    /// Monday first.
    pub days: [bool; 7],
    pub every: u32,
    pub hours: bool,
    pub typed: String,
    /// The text it last read or wrote: when the form's differs, it's read again.
    synced: String,
}

impl SchedulePicker {
    pub fn from_text(text: &str) -> Self {
        let mut picker = Self {
            kind: Kind::Manual,
            time: "02:00".into(),
            days: [true, false, false, false, false, false, false],
            every: 6,
            hours: true,
            typed: text.to_owned(),
            synced: text.to_owned(),
        };
        let time = |t: &chrono::NaiveTime| format!("{:02}:{:02}", t.hour(), t.minute());
        if text.trim().is_empty() {
            return picker;
        }
        match text.parse::<Schedule>() {
            Ok(Schedule::Daily { at }) => {
                picker.kind = Kind::Daily;
                picker.time = time(&at);
            }
            Ok(Schedule::Weekly { days, at }) => {
                picker.kind = Kind::Weekly;
                picker.time = time(&at);
                picker.days = DAYS.map(|(d, ..)| days.contains(&d));
            }
            Ok(Schedule::Every { interval }) => {
                picker.kind = Kind::Every;
                let minutes = interval.num_minutes();
                if minutes % 60 == 0 {
                    picker.every = (minutes / 60) as u32;
                } else {
                    picker.every = minutes as u32;
                    picker.hours = false;
                }
            }
            Err(_) => picker.kind = Kind::Custom,
        }
        picker
    }

    /// The schedule's text, as core reads it (or as typed).
    pub fn to_text(&self) -> String {
        let time = self.time.trim();
        match self.kind {
            Kind::Manual => String::new(),
            Kind::Daily => format!("daily {time}"),
            Kind::Weekly => {
                let days: Vec<&str> = DAYS
                    .iter()
                    .zip(self.days)
                    .filter(|(_, on)| *on)
                    .map(|((_, name, _), _)| *name)
                    .collect();
                format!("weekly {} {time}", days.join(","))
            }
            Kind::Every => format!("every {}{}", self.every, if self.hours { "h" } else { "m" }),
            Kind::Custom => self.typed.trim().to_owned(),
        }
    }

    /// Shows the picker's rows; writes `text` when they change it.
    pub fn ui(&mut self, ui: &mut Ui, text: &mut String) {
        if *text != self.synced {
            *self = Self::from_text(text);
        }
        let before = self.clone();
        setting_row(ui, &tr("editor-schedule-runs"), None, |ui| {
            let current = KINDS
                .iter()
                .find(|(k, _)| *k == self.kind)
                .map_or_else(String::new, |(_, key)| tr(key));
            egui::ComboBox::from_id_salt("schedule-kind")
                .selected_text(current)
                .width(200.0)
                .show_ui(ui, |ui| {
                    for (kind, key) in KINDS {
                        dropdown_value(ui, &mut self.kind, kind, tr(key));
                    }
                })
                .response
        });
        match self.kind {
            Kind::Manual => {}
            Kind::Daily => {
                self.time_row(ui);
            }
            Kind::Weekly => {
                setting_row(ui, &tr("editor-schedule-days"), None, |ui| {
                    // Right to left: Sunday first, so they read Monday first.
                    let mut last = None;
                    for (i, (_, _, key)) in DAYS.iter().enumerate().rev() {
                        let r = ui.toggle_value(&mut self.days[i], tr(key));
                        last = Some(r);
                    }
                    last.expect("seven days")
                });
                self.time_row(ui);
            }
            Kind::Every => {
                setting_row(ui, &tr("editor-schedule-interval"), None, |ui| {
                    let unit = if self.hours {
                        tr("editor-schedule-hours")
                    } else {
                        tr("editor-schedule-minutes")
                    };
                    egui::ComboBox::from_id_salt("schedule-unit")
                        .selected_text(unit)
                        .width(100.0)
                        .show_ui(ui, |ui| {
                            dropdown_value(ui, &mut self.hours, true, tr("editor-schedule-hours"));
                            dropdown_value(
                                ui,
                                &mut self.hours,
                                false,
                                tr("editor-schedule-minutes"),
                            );
                        })
                        .response
                        .accessible_name(tr("editor-schedule-unit"));
                    ui.add_space(space::XS);
                    ui.add(egui::DragValue::new(&mut self.every).range(1..=10_000))
                });
            }
            Kind::Custom => {
                setting_row(
                    ui,
                    &tr("editor-schedule-text"),
                    Some(&tr("editor-schedule-hint")),
                    |ui| ui.add(egui::TextEdit::singleline(&mut self.typed).desired_width(240.0)),
                );
            }
        }
        if *self != before {
            *text = self.to_text();
            self.synced = text.clone();
        }
    }

    fn time_row(&mut self, ui: &mut Ui) -> Response {
        setting_row(ui, &tr("editor-schedule-at"), None, |ui| {
            ui.add(egui::TextEdit::singleline(&mut self.time).desired_width(72.0))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_reads_back_what_it_writes() {
        for text in [
            "",
            "daily 02:30",
            "weekly mon,thu 21:00",
            "every 6h",
            "every 90m",
        ] {
            let picker = SchedulePicker::from_text(text);
            assert_ne!(picker.kind, Kind::Custom, "{text}");
            assert_eq!(picker.to_text(), text);
        }
    }

    #[test]
    fn text_it_can_t_show_is_kept_as_typed() {
        let picker = SchedulePicker::from_text("now and then");
        assert_eq!(picker.kind, Kind::Custom);
        assert_eq!(picker.to_text(), "now and then");
    }

    #[test]
    fn switching_kind_writes_core_s_form() {
        let mut picker = SchedulePicker::from_text("");
        picker.kind = Kind::Daily;
        picker.time = "7:05".into();
        assert_eq!(picker.to_text(), "daily 7:05");
        assert!(picker.to_text().parse::<Schedule>().is_ok());
        picker.kind = Kind::Weekly;
        picker.days = [false, true, false, false, false, true, false];
        assert_eq!(picker.to_text(), "weekly tue,sat 7:05");
    }
}
