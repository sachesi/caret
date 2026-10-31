//! Translations: binding the catalogue, and messages with named placeholders.

/// The catalogue's name, and where the catalogues are installed.
const DOMAIN: &str = "caret";
const LOCALEDIR: &str = match option_env!("CARET_LOCALEDIR") {
    Some(directory) => directory,
    None => "/usr/local/share/locale",
};

/// Sets the locale from the environment and binds the catalogue.
///
/// # Safety
///
/// Call it first in `main`, before any thread is started: setting the locale is unsound
/// while another thread reads it.
#[expect(unsafe_code, reason = "setting the locale is unsound once threads run")]
pub unsafe fn init() {
    // SAFETY: the caller has started no thread yet.
    unsafe { gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "") };
    if let Err(error) = gettextrs::bindtextdomain(DOMAIN, LOCALEDIR)
        .and_then(|_| gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8"))
        .and_then(|_| gettextrs::textdomain(DOMAIN))
    {
        tracing::warn!("binding the translations in {LOCALEDIR}: {error}");
    }
}

/// `gettext` for a message with named placeholders, which a translation may reorder:
/// `gettext_f("Could not open the link: {reason}", &[("reason", &message)])`.
pub fn gettext_f(msgid: &str, args: &[(&str, &str)]) -> String {
    fill(&gettextrs::gettext(msgid), args)
}

/// `ngettext` with named placeholders; the count is usually one of them as well.
pub fn ngettext_f(msgid: &str, msgid_plural: &str, n: u32, args: &[(&str, &str)]) -> String {
    fill(&gettextrs::ngettext(msgid, msgid_plural, n), args)
}

/// Replaces each `{name}` in one pass, so a value that contains braces is never
/// substituted again. Unknown names stay as they are.
fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut filled = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        filled.push_str(&rest[..open]);
        rest = &rest[open..];
        let found = rest.find('}').and_then(|close| {
            let name = &rest[1..close];
            args.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (close, *value))
        });
        if let Some((close, value)) = found {
            filled.push_str(value);
            rest = &rest[close + 1..];
        } else {
            filled.push('{');
            rest = &rest[1..];
        }
    }
    filled.push_str(rest);
    filled
}

#[cfg(test)]
mod tests {
    use super::fill;

    #[test]
    fn placeholders_follow_the_translation_order() {
        let args = [("program", "htop"), ("reason", "not found")];
        assert_eq!(fill("{reason}: «{program}»", &args), "not found: «htop»");
    }

    #[test]
    fn values_are_not_substituted_again() {
        let args = [("program", "echo {reason}"), ("reason", "not found")];
        assert_eq!(
            fill("Could not start “{program}”: {reason}", &args),
            "Could not start “echo {reason}”: not found"
        );
    }

    #[test]
    fn unknown_and_unclosed_braces_stay() {
        assert_eq!(
            fill("{missing} and {", &[("program", "x")]),
            "{missing} and {"
        );
    }
}
