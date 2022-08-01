use log::{error, info, warn};

use std::io;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::auth::{AuthUserInfo, AuthenticationError};
use crate::config::{Config, FocusBehaviour};
use crate::info_caching::{get_cached_username, set_cached_username};
use crate::post_login::{EnvironmentStartError, PostLoginEnvironment};
use status_message::StatusMessage;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use tui::{backend::Backend, Frame, Terminal};

mod chunks;
mod input_field;
mod power_menu;
mod status_message;
mod switcher;

use chunks::Chunks;
use input_field::{InputFieldDisplayType, InputFieldWidget};
use power_menu::PowerMenuWidget;
use status_message::{ErrorStatusMessage, InfoStatusMessage};
use switcher::{SwitcherItem, SwitcherWidget};

/// All the different modes for input
#[derive(Clone, Copy)]
enum InputMode {
    /// Using the env switcher widget
    Switcher,

    /// Typing within the Username input field
    Username,

    /// Typing within the Password input field
    Password,

    /// Nothing selected
    Normal,
}

impl InputMode {
    /// Move to the next mode
    fn next(&mut self) {
        use InputMode::*;

        *self = match self {
            Normal => Switcher,
            Switcher => Username,
            Username => Password,
            Password => Password,
        }
    }

    /// Move to the previous mode
    fn prev(&mut self) {
        use InputMode::*;

        *self = match self {
            Normal => Normal,
            Switcher => Normal,
            Username => Switcher,
            Password => Username,
        }
    }
}

enum UIThreadRequest {
    Redraw,
    StopDrawing,
}

/// App holds the state of the application
#[derive(Clone)]
pub struct LoginForm {
    /// Whether the application is running in preview mode
    preview: bool,
    power_menu_widget: PowerMenuWidget,
    switcher_widget: SwitcherWidget<PostLoginEnvironment>,
    username_widget: InputFieldWidget,
    password_widget: InputFieldWidget,

    /// Current input mode
    input_mode: InputMode,

    /// Message that is displayed
    status_message: Option<StatusMessage>,

    /// The configuration for the app
    config: Config,

    /// Used for the event thread to send redraw and terminate requests
    send_redraw_channel: Option<Sender<UIThreadRequest>>,
}

impl LoginForm {
    fn try_redraw(&mut self) {
        if let Some(ui_thread_channel) = &self.send_redraw_channel {
            match ui_thread_channel.send(UIThreadRequest::Redraw) {
                Ok(_) => {}
                Err(err) => warn!("Failed to redraw. Reason: {}", err),
            }
        }
    }

    fn set_status_message(&mut self, status: impl Into<StatusMessage>) {
        let status = status.into();
        self.status_message = Some(status);
        self.try_redraw();
    }

    fn clear_status_message(&mut self) {
        self.status_message = None;
        self.try_redraw();
    }

    pub fn new(config: Config, preview: bool) -> LoginForm {
        let remember_username = config.username_field.remember_username;

        let preset_username = if remember_username {
            get_cached_username()
        } else {
            None
        };

        LoginForm {
            preview,
            power_menu_widget: PowerMenuWidget::new(config.power_controls.clone()),
            switcher_widget: SwitcherWidget::new(
                crate::post_login::get_envs()
                    .into_iter()
                    .map(|(title, content)| SwitcherItem::new(title, content))
                    .collect(),
                config.environment_switcher.clone(),
            ),
            username_widget: InputFieldWidget::new(
                InputFieldDisplayType::Echo,
                config.username_field.style.clone(),
                preset_username.clone().unwrap_or_default(),
            ),
            password_widget: InputFieldWidget::new(
                InputFieldDisplayType::Replace(
                    config
                        .password_field
                        .content_replacement_character
                        .to_string(),
                ),
                config.password_field.style.clone(),
                String::default(),
            ),
            input_mode: match config.focus_behaviour {
                FocusBehaviour::NoFocus => InputMode::Normal,
                FocusBehaviour::Environment => InputMode::Switcher,
                FocusBehaviour::Username => InputMode::Username,
                FocusBehaviour::Password => InputMode::Password,
                FocusBehaviour::FirstNonCached => match preset_username {
                    Some(_) => InputMode::Password,
                    None => InputMode::Username,
                },
            },
            status_message: None,
            config,
            send_redraw_channel: None,
        }
    }

    pub fn run<'a, B, A, S>(
        self,
        terminal: &mut Terminal<B>,
        auth_fn: A,
        start_env_fn: S,
    ) -> io::Result<()>
    where
        B: Backend,
        A: Fn(String, String) -> Result<AuthUserInfo<'a>, AuthenticationError>
            + std::marker::Send
            + 'static,
        S: Fn(&PostLoginEnvironment, &Config, &AuthUserInfo) -> Result<(), EnvironmentStartError>
            + std::marker::Send
            + 'static,
    {
        let login_form = Arc::new(Mutex::new(self));
        match terminal.draw(|f| {
            let layout = Chunks::new(f);
            let mut login_form = match login_form.lock() {
                Ok(guard) => guard,
                Err(err) => {
                    error!("Lock failed. Reason: {}", err);
                    std::process::exit(1);
                }
            };
            login_form.render(f, layout);
        }) {
            Ok(_) => {}
            Err(err) => {
                error!("Failed to draw. Reason: {}", err);
                std::process::exit(1);
            }
        }

        let (req_send_channel, req_recv_channel) = channel();

        let event_login_form = login_form.clone();
        std::thread::spawn(move || {
            {
                let mut login_form = match event_login_form.lock() {
                    Ok(guard) => guard,
                    Err(err) => {
                        error!("Lock failed. Reason: {}", err);
                        std::process::exit(1);
                    }
                };
                login_form.send_redraw_channel = Some(req_send_channel.clone());
            }

            loop {
                if let Ok(Event::Key(key)) = event::read() {
                    let mut login_form = match event_login_form.lock() {
                        Ok(guard) => guard,
                        Err(err) => {
                            error!("Lock failed. Reason: {}", err);
                            std::process::exit(1);
                        }
                    };
                    match (key.code, &login_form.input_mode) {
                        (KeyCode::Enter, &InputMode::Password) => {
                            if login_form.preview {
                                login_form.set_status_message(InfoStatusMessage::Authenticating);
                                std::thread::sleep(Duration::from_secs(2));
                                login_form.set_status_message(InfoStatusMessage::LoggingIn);
                                std::thread::sleep(Duration::from_secs(2));
                                login_form.clear_status_message();
                            } else {
                                login_form.attempt_login(&auth_fn, &start_env_fn);
                            }
                        }
                        (KeyCode::Enter | KeyCode::Down, _) => {
                            login_form.input_mode.next();
                        }
                        (KeyCode::Up, _) => {
                            login_form.input_mode.prev();
                        }
                        (KeyCode::Tab, _) => {
                            if key.modifiers == KeyModifiers::SHIFT {
                                login_form.input_mode.prev();
                            } else {
                                login_form.input_mode.next();
                            }
                        }

                        // Esc is the overal key to get out of your input mode
                        (KeyCode::Esc, InputMode::Normal) => {
                            if login_form.preview {
                                info!("Pressed escape in preview mode to exit the application");
                                req_send_channel.send(UIThreadRequest::StopDrawing).unwrap();
                            }
                        }

                        (KeyCode::Esc, _) => {
                            login_form.input_mode = InputMode::Normal;
                        }

                        // For the different input modes the key should be passed to the corresponding
                        // widget.
                        (k, mode) => {
                            let status_message_opt = match *mode {
                                InputMode::Switcher => login_form.switcher_widget.key_press(k),
                                InputMode::Username => login_form.username_widget.key_press(k),
                                InputMode::Password => login_form.password_widget.key_press(k),
                                InputMode::Normal => login_form.power_menu_widget.key_press(k),
                            };

                            // We don't wanna clear any existing error messages
                            if let Some(status_msg) = status_message_opt {
                                login_form.set_status_message(status_msg);
                            }
                        }
                    };
                }

                {
                    let mut login_form = match event_login_form.lock() {
                        Ok(guard) => guard,
                        Err(err) => {
                            error!("Lock failed. Reason: {}", err);
                            std::process::exit(1);
                        }
                    };
                    login_form.try_redraw();
                }
            }
        });

        // Start the UI thread. This actually draws to the screen.
        //
        // This blocks until we actually call StopDrawing
        while let UIThreadRequest::Redraw = req_recv_channel.recv().unwrap() {
            terminal
                .draw(|f| {
                    let layout = Chunks::new(f);
                    let mut login_form = match login_form.lock() {
                        Ok(guard) => guard,
                        Err(err) => {
                            error!("Lock failed. Reason: {}", err);
                            std::process::exit(1);
                        }
                    };
                    login_form.render(f, layout);
                })
                .unwrap();
        }

        Ok(())
    }

    fn render<B: Backend>(&mut self, frame: &mut Frame<B>, chunks: Chunks) {
        self.power_menu_widget.render(frame, chunks.power_menu);
        self.switcher_widget.render(
            frame,
            chunks.switcher,
            matches!(self.input_mode, InputMode::Switcher),
        );
        self.username_widget.render(
            frame,
            chunks.username_field,
            matches!(self.input_mode, InputMode::Username),
        );
        self.password_widget.render(
            frame,
            chunks.password_field,
            matches!(self.input_mode, InputMode::Password),
        );

        // Display Status Message
        StatusMessage::render(self.status_message, frame, chunks.status_message);
    }

    fn attempt_login<'a, A, S>(&mut self, auth_fn: A, start_env_fn: S)
    where
        A: Fn(String, String) -> Result<AuthUserInfo<'a>, AuthenticationError>,
        S: Fn(&PostLoginEnvironment, &Config, &AuthUserInfo) -> Result<(), EnvironmentStartError>,
    {
        let username = self.username_widget.get_content();
        let password = self.password_widget.get_content();

        // Fetch the selected post login environment
        let post_login_env = match self.switcher_widget.selected() {
            None => {
                self.set_status_message(ErrorStatusMessage::NoGraphicalEnvironment);
                return;
            }
            Some(selected) => selected,
        }
        .content
        .clone();

        self.set_status_message(InfoStatusMessage::Authenticating);
        let user_info = match auth_fn(username.clone(), password) {
            Err(err) => {
                self.set_status_message(ErrorStatusMessage::AuthenticationError(err));

                // Clear the password field
                self.password_widget.clear();

                return;
            }
            Ok(res) => res,
        };

        // Remember username for next time
        if self.config.username_field.remember_username {
            set_cached_username(&username);
        }

        self.set_status_message(InfoStatusMessage::LoggingIn);

        // NOTE: if this call is succesful, it blocks the thread until the environment is
        // terminated
        start_env_fn(&post_login_env, &self.config, &user_info).unwrap_or_else(|_| {
            error!("Starting post-login environment failed");
            self.set_status_message(ErrorStatusMessage::FailedGraphicalEnvironment);
        });

        self.clear_status_message();

        // Just to add explicitness that the user session is dropped here
        drop(user_info);
    }
}
