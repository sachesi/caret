//! The connection to the terminal server: requests go out through a writer thread, and
//! what the server sends comes back on the main context to the window or terminal view
//! it is for.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;
use std::sync::mpsc;

use caret_protocol::{Answer, Message, Question, Request, SessionId, TerminalId};
use gtk::glib;
use gtk::prelude::*;

use crate::config;
use crate::terminal_view::TerminalView;
use crate::window::Window;

/// What is called with the server's answer to a question.
type Answered = Box<dyn FnOnce(Answer)>;

pub struct Connection {
    writer: mpsc::Sender<Vec<u8>>,
    next_question: Cell<u32>,
    questions: RefCell<HashMap<u32, Answered>>,
    views: RefCell<HashMap<TerminalId, glib::WeakRef<TerminalView>>>,
    windows: RefCell<HashMap<SessionId, glib::WeakRef<Window>>>,
    /// Why the server is out of reach, once it is.
    lost: RefCell<Option<String>>,
}

thread_local! {
    static CONNECTION: RefCell<Option<Result<Rc<Connection>, String>>> =
        const { RefCell::new(None) };
}

/// The connection of this process, made the first time; the error says why there is
/// none.
pub fn connection() -> Result<Rc<Connection>, String> {
    CONNECTION.with(|cell| {
        cell.borrow_mut()
            .get_or_insert_with(|| Connection::open().map(Rc::new))
            .clone()
    })
}

impl Connection {
    fn open() -> Result<Self, String> {
        // Version 1: this window shows a tab of split panes as its first pane.
        let (mut stream, _) = caret_protocol::client::connect(config::APPLICATION_ID, 1)
            .map_err(|error| error.to_string())?;
        let mut out = stream.try_clone().map_err(|error| error.to_string())?;

        let (writer, written) = mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("server writer".to_owned())
            .spawn(move || {
                for bytes in written {
                    if out.write_all(&bytes).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| error.to_string())?;

        let (messages, received) = async_channel::unbounded();
        std::thread::Builder::new()
            .name("server reader".to_owned())
            .spawn(move || {
                let reason = loop {
                    match caret_protocol::receive::<Message>(&mut stream) {
                        Ok(Some(message)) => {
                            if messages.send_blocking(Ok(message)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => break "the server closed the connection".to_owned(),
                        Err(error) => break error.to_string(),
                    }
                };
                let _ = messages.send_blocking(Err(reason));
            })
            .map_err(|error| error.to_string())?;

        glib::spawn_future_local(async move {
            while let Ok(message) = received.recv().await {
                let Ok(connection) = connection() else { break };
                match message {
                    Ok(message) => connection.dispatch(message),
                    Err(reason) => {
                        connection.lose(&reason);
                        break;
                    }
                }
            }
        });

        Ok(Self {
            writer,
            next_question: Cell::new(1),
            questions: RefCell::default(),
            views: RefCell::default(),
            windows: RefCell::default(),
            lost: RefCell::default(),
        })
    }

    pub fn send(&self, request: &Request) {
        let mut bytes = Vec::new();
        // Writing into a vector cannot fail; a message past 4 GiB would.
        let _ = caret_protocol::send(&mut bytes, request);
        // Fails once the writer has ended, which the reader then reports.
        let _ = self.writer.send(bytes);
    }

    /// Has `answered` called with the server's answer to `question`, in order with the
    /// rest of what the server sends; `Gone` if the connection is lost first.
    pub fn ask(&self, question: Question, answered: impl FnOnce(Answer) + 'static) {
        if self.lost.borrow().is_some() {
            answered(Answer::Gone);
            return;
        }
        let id = self.next_question.get();
        self.next_question.set(id.wrapping_add(1));
        self.questions.borrow_mut().insert(id, Box::new(answered));
        self.send(&Request::Ask { id, question });
    }

    pub fn register_view(&self, terminal: TerminalId, view: &TerminalView) {
        self.views.borrow_mut().insert(terminal, view.downgrade());
    }

    pub fn forget_view(&self, terminal: TerminalId) {
        self.views.borrow_mut().remove(&terminal);
    }

    pub fn register_window(&self, session: SessionId, window: &Window) {
        self.windows
            .borrow_mut()
            .insert(session, window.downgrade());
    }

    pub fn forget_window(&self, session: SessionId) {
        self.windows.borrow_mut().remove(&session);
    }

    fn view(&self, terminal: TerminalId) -> Option<TerminalView> {
        self.views.borrow().get(&terminal)?.upgrade()
    }

    fn window(&self, session: SessionId) -> Option<Window> {
        self.windows.borrow().get(&session)?.upgrade()
    }

    #[expect(clippy::too_many_lines, reason = "one arm for each message")]
    fn dispatch(&self, message: Message) {
        match message {
            Message::Answer { id, answer } => {
                let waiting = self.questions.borrow_mut().remove(&id);
                if let Some(answered) = waiting {
                    answered(answer);
                }
            }
            Message::Attached { session, tabs } => {
                if let Some(window) = self.window(session) {
                    window.attached(tabs);
                }
            }
            Message::TabOpened {
                session,
                index,
                tab,
            } => {
                if let Some(window) = self.window(session) {
                    window.tab_opened(index, tab);
                }
            }
            Message::TabClosed { terminal, .. } => {
                if let Some(view) = self.view(terminal) {
                    view.closed();
                }
            }
            Message::TabMoved {
                session,
                terminal,
                index,
            } => {
                if let Some(window) = self.window(session) {
                    window.tab_moved(terminal, index);
                }
            }
            Message::SessionClosed { session } => {
                if let Some(window) = self.window(session) {
                    window.session_closed();
                }
            }
            Message::TabNamed { terminal, name } => {
                if let Some(view) = self.view(terminal) {
                    view.named(name);
                }
            }
            Message::Frame { terminal, frame } => {
                if let Some(view) = self.view(terminal) {
                    view.frame(frame);
                }
            }
            Message::Title { terminal, title } => {
                if let Some(view) = self.view(terminal) {
                    view.set_title(&title);
                }
            }
            Message::Bell { terminal } => {
                if let Some(view) = self.view(terminal) {
                    view.bell();
                }
            }
            Message::Clipboard {
                terminal,
                primary,
                text,
            } => {
                if let Some(view) = self.view(terminal) {
                    let clipboard = if primary {
                        view.primary_clipboard()
                    } else {
                        view.clipboard()
                    };
                    clipboard.set_text(&text);
                }
            }
            Message::Notification {
                terminal,
                title,
                body,
            } => {
                if let Some(view) = self.view(terminal) {
                    view.program_notification(&title, &body);
                }
            }
            Message::CommandStarted { terminal } => {
                if let Some(view) = self.view(terminal) {
                    view.command_started();
                }
            }
            Message::CommandFinished {
                terminal,
                seconds,
                status,
            } => {
                if let Some(view) = self.view(terminal) {
                    view.command_finished(seconds, status);
                }
            }
            Message::Failed { terminal, reason } => {
                if let Some(view) = self.view(terminal) {
                    view.failed(&reason);
                }
            }
            Message::Ended {
                terminal,
                code,
                signal,
            } => {
                if let Some(view) = self.view(terminal) {
                    view.ended(code, signal);
                }
            }
            // Not sent in version 1, which this window speaks.
            Message::Layout { .. }
            | Message::SessionNamed { .. }
            | Message::ShowSession { .. }
            | Message::SessionColored { .. } => {}
        }
    }

    /// The server is gone: every view says so, and questions get no answer.
    fn lose(&self, reason: &str) {
        tracing::warn!("lost the terminal server: {reason}");
        self.lost.replace(Some(reason.to_owned()));
        let waiting: Vec<_> = self.questions.borrow_mut().drain().collect();
        for (_, answered) in waiting {
            answered(Answer::Gone);
        }
        let views: Vec<TerminalView> = self
            .views
            .borrow()
            .values()
            .filter_map(glib::WeakRef::upgrade)
            .collect();
        for view in views {
            view.server_lost(reason);
        }
    }
}
