//! Preferences dialog: every row writes its GSettings key immediately.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::gtk::pango;
use crate::settings::{self, settings};
use crate::{adw, gio, glib, gtk};

pub fn preferences_dialog() -> adw::PreferencesDialog {
    let dialog = adw::PreferencesDialog::builder()
        .title(gettext("Preferences"))
        .search_enabled(false)
        .build();
    let settings = settings();
    let page = adw::PreferencesPage::builder()
        .title(gettext("General"))
        .icon_name("preferences-system-symbolic")
        .build();

    let text = adw::PreferencesGroup::builder()
        .title(gettext("Text"))
        .build();
    let system_font = adw::SwitchRow::builder()
        .title(gettext("Use the System Font"))
        .active(settings.string("font").is_empty())
        .build();
    let font_button = gtk::FontDialogButton::builder()
        .dialog(
            &gtk::FontDialog::builder()
                .title(gettext("Terminal Font"))
                .filter(&gtk::CustomFilter::new(|item| {
                    // The dialog lists families and then their faces.
                    item.downcast_ref::<pango::FontFamily>()
                        .map(|family| family.is_monospace())
                        .or_else(|| {
                            item.downcast_ref::<pango::FontFace>()
                                .map(|face| face.family().is_monospace())
                        })
                        .unwrap_or(false)
                }))
                .build(),
        )
        .level(gtk::FontLevel::Font)
        .font_desc(&settings::font())
        .valign(gtk::Align::Center)
        .build();
    let font_row = adw::ActionRow::builder()
        .title(gettext("Font"))
        .activatable_widget(&font_button)
        .build();
    font_row.add_suffix(&font_button);
    system_font
        .bind_property("active", &font_row, "sensitive")
        .invert_boolean()
        .sync_create()
        .build();
    // Set while the rows follow the key, so that showing the font does not write it back.
    let following = Rc::new(Cell::new(false));
    system_font.connect_active_notify(glib::clone!(
        #[strong]
        settings,
        #[strong]
        following,
        #[weak]
        font_button,
        move |row| {
            if following.get() {
                return;
            }
            let font = if row.is_active() {
                String::new()
            } else {
                font_button
                    .font_desc()
                    .map(|font| font.to_str().to_string())
                    .unwrap_or_default()
            };
            let _ = settings.set_string("font", &font);
        }
    ));
    font_button.connect_font_desc_notify(glib::clone!(
        #[strong]
        settings,
        #[strong]
        following,
        #[weak]
        system_font,
        move |button| {
            if !following.get()
                && !system_font.is_active()
                && let Some(font) = button.font_desc()
            {
                let _ = settings.set_string("font", &font.to_str());
            }
        }
    ));
    // The rows follow the key when something else changes it, and stop when the dialog
    // goes.
    let handler = settings.connect_changed(
        Some("font"),
        glib::clone!(
            #[weak]
            system_font,
            #[weak]
            font_button,
            move |settings, _| {
                following.set(true);
                font_button.set_font_desc(&settings::font());
                system_font.set_active(settings.string("font").is_empty());
                following.set(false);
            }
        ),
    );
    let handler = RefCell::new(Some(handler));
    dialog.connect_closed(glib::clone!(
        #[strong]
        settings,
        move |_| {
            if let Some(handler) = handler.take() {
                settings.disconnect(handler);
            }
        }
    ));
    text.add(&system_font);
    text.add(&font_row);
    page.add(&text);

    let appearance = adw::PreferencesGroup::builder()
        .title(gettext("Appearance"))
        .build();
    appearance.add(&choice_row(
        &settings,
        "color-scheme",
        &gettext("Style"),
        &[
            ("system", gettext("Follow System")),
            ("light", gettext("Light")),
            ("dark", gettext("Dark")),
        ],
    ));
    appearance.add(&choice_row(
        &settings,
        "cursor-shape",
        &gettext("Cursor Shape"),
        &[
            ("block", gettext("Block")),
            ("beam", gettext("I-Beam")),
            ("underline", gettext("Underline")),
        ],
    ));
    page.add(&appearance);

    let behaviour = adw::PreferencesGroup::builder()
        .title(gettext("Behaviour"))
        .build();
    let scrollback = adw::SpinRow::builder()
        .title(gettext("Lines of History"))
        .subtitle(gettext(
            "Kept for scrolling back after they leave the screen",
        ))
        .adjustment(&gtk::Adjustment::new(
            0.0,
            0.0,
            1_000_000.0,
            1000.0,
            10_000.0,
            0.0,
        ))
        .build();
    settings
        .bind("scrollback-lines", &scrollback, "value")
        .build();
    behaviour.add(&scrollback);
    let bell = adw::SwitchRow::builder()
        .title(gettext("Terminal Bell"))
        .subtitle(gettext("Sound the bell when a program rings it"))
        .build();
    settings.bind("audible-bell", &bell, "active").build();
    behaviour.add(&bell);
    page.add(&behaviour);

    dialog.add(&page);
    dialog
}

/// A row choosing one of the values of an enum key, given as its nicks and their labels.
fn choice_row(
    settings: &gio::Settings,
    key: &str,
    title: &str,
    choices: &[(&'static str, String)],
) -> adw::ComboRow {
    let labels: Vec<&str> = choices.iter().map(|(_, label)| label.as_str()).collect();
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(&labels))
        .build();
    let nicks: Vec<&'static str> = choices.iter().map(|&(nick, _)| nick).collect();
    let to_row = nicks.clone();
    settings
        .bind(key, &row, "selected")
        .mapping(move |value, _| {
            let nick = value.str()?;
            let index = to_row.iter().position(|&choice| choice == nick)?;
            Some(u32::try_from(index).ok()?.to_value())
        })
        .set_mapping(move |value, _| {
            let index = usize::try_from(value.get::<u32>().ok()?).ok()?;
            nicks.get(index).map(|nick| nick.to_variant())
        })
        .build();
    row
}
