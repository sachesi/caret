use super::*;

impl TerminalView {
    /// Selects the next place `text` appears, upwards through the history or back down,
    /// and scrolls to it, which the server does a few thousand lines at a time; then
    /// `done` hears whether it appears anywhere. A later search or a cleared selection
    /// cancels it, and then `done` is never called.
    pub fn find(&self, text: &str, upwards: bool, done: impl FnOnce(bool) + 'static) {
        let imp = self.imp();
        let search = imp.searches.get() + 1;
        imp.searches.set(search);
        let (Some(terminal), Ok(connection)) = (self.terminal(), connection()) else {
            done(false);
            return;
        };
        let question = Question::Find {
            terminal,
            text: text.to_owned(),
            upwards,
        };
        let weak = self.downgrade();
        connection.ask(question, move |answer| {
            let Some(view) = weak.upgrade() else { return };
            if view.imp().searches.get() != search {
                return;
            }
            match answer {
                Answer::Found { found } => done(found),
                Answer::Gone => {}
                _ => done(false),
            }
        });
    }
}
