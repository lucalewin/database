use std::cell::Cell;

use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Role, StatefulInteractiveElement as _, Styled as _, Window,
    component::{
        ActiveTheme as _, Disableable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        checkbox::Checkbox,
        dialog::DialogFooter,
        form::{Field, Form},
        input::{Input, InputContentType, InputState},
        notification::Notification,
        radio::RadioGroup,
    },
    div,
    prelude::FluentBuilder as _,
};

use crate::session::{ConnectionTier, Session};

struct ConnectionDialog {
    explorer: Entity<Session>,
    name: Entity<InputState>,
    url: Entity<InputState>,
    password: Entity<InputState>,
    tier: ConnectionTier,
    disable_tls: bool,
    error: Option<String>,
    saving: bool,
}

impl ConnectionDialog {
    fn new(explorer: Entity<Session>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Connection name"));
        let url = cx.new(|cx| {
            InputState::new(window, cx).placeholder("postgresql://user@host:5432/database")
        });
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        Self {
            explorer,
            name,
            url,
            password,
            tier: ConnectionTier::Local,
            disable_tls: false,
            error: None,
            saving: false,
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let name = self.name.read(cx).value().to_string();
        let url = self.url.read(cx).value().to_string();
        let password = self.password.read(cx).value().to_string();
        let task = match self.explorer.read(cx).begin_add_connection(
            name,
            url,
            password,
            self.tier,
            self.disable_tls,
        ) {
            Ok(task) => task,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.saving = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|_| Err("Could not save the connection".into()));
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Ok((profiles, id)) => {
                        let name = profiles
                            .iter()
                            .find(|profile| profile.id() == id)
                            .map(|profile| profile.name().to_owned());
                        this.explorer.update(cx, |state, cx| {
                            state.finish_add_connection(profiles, &id, cx)
                        });
                        window.close_dialog(cx);
                        let message = name.map_or_else(
                            || "The connection was saved.".to_owned(),
                            |name| format!("“{name}” was saved."),
                        );
                        window.push_notification(
                            Notification::success(message).title("Connection added"),
                            cx,
                        );
                    }
                    Err(error) => {
                        this.error = Some(error);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
}

impl Render for ConnectionDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = match self.tier {
            ConnectionTier::Local => 0,
            ConnectionTier::Staging => 1,
            ConnectionTier::Production => 2,
        };
        div().flex().flex_col().gap_3()
            .child(Form::new()
                .child(Field::new().label("Name").required(true).child(Input::new(&self.name).id("connection-name").aria_label("Connection name").disabled(self.saving)))
                .child(Field::new().label("PostgreSQL URL").required(true)
                    .description("Enter the password separately, not in the URL.")
                    .child(Input::new(&self.url).id("connection-url").aria_label("PostgreSQL URL").disabled(self.saving)))
                .child(Field::new().label("Password")
                    .description("Stored in the system keyring. Leave blank to use PostgreSQL's configured authentication.")
                    .child(Input::new(&self.password).id("connection-password").aria_label("Password").content_type(InputContentType::Password).disabled(self.saving)))
                .child(Field::new().label("Environment")
                    .description("Production blocks editing and SQL execution; use database permissions for security.")
                    .child(RadioGroup::horizontal("connection-tier").children(["Local", "Staging", "Production"])
                        .selected_index(Some(selected))
                        .disabled(self.saving)
                        .on_change(cx.listener(|this, ix, _, cx| {
                            this.tier = match *ix { 0 => ConnectionTier::Local, 1 => ConnectionTier::Staging, _ => ConnectionTier::Production };
                            if this.tier != ConnectionTier::Local { this.disable_tls = false; }
                            cx.notify();
                        }))))
                .child(Field::new().label_indent(false)
                    .visible(self.tier == ConnectionTier::Local)
                    .description("Without TLS, credentials and data are sent unencrypted.")
                    .child(Checkbox::new("disable-tls").label("Disable TLS for this Local connection")
                        .checked(self.disable_tls)
                        .disabled(self.saving)
                        .on_change(cx.listener(|this, value, _, cx| { this.disable_tls = *value; cx.notify(); })))))
            .when(self.saving, |view| view.child(div().text_sm().child("Saving connection…")))
            .when_some(self.error.clone(), |view, error| view.child(div().id("connection-error").role(Role::Alert).aria_label(error.clone()).text_sm().text_color(cx.theme().danger).child(error)))
    }
}

pub fn open_connection_dialog(explorer: Entity<Session>, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| ConnectionDialog::new(explorer, window, cx));
    let name = form.read(cx).name.clone();
    let initial_focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) {
            name.focus_handle(cx).focus(window, cx);
        }
        let confirm_form = form.clone();
        let cancel_form = form.clone();
        let button_form = form.clone();
        let cancel_button_form = form.clone();
        dialog
            .title("Add connection")
            .overlay_closable(false)
            .close_button(false)
            .child(form.clone())
            .on_ok(move |_, window, cx| {
                confirm_form.update(cx, |form, cx| form.save(window, cx));
                false
            })
            .on_cancel(move |_, _, cx| !cancel_form.read(cx).saving)
            .footer(
                DialogFooter::new()
                    .child(Button::new("cancel-connection").label("Cancel").on_click(
                        move |_, window, cx| {
                            if !cancel_button_form.read(cx).saving {
                                window.close_dialog(cx);
                            }
                        },
                    ))
                    .child(
                        Button::new("save-connection")
                            .primary()
                            .label("Add connection")
                            .on_click(move |_, window, cx| {
                                button_form.update(cx, |form, cx| form.save(window, cx));
                            }),
                    ),
            )
    });
}
