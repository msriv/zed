use gpui::{Task, WindowHandle};
use settings::Settings;
use twg::TwgSettings;
use ui::prelude::*;
use util::ResultExt;
use workspace::MultiWorkspace;

use crate::SettingsWindow;

pub(crate) fn render_twg_connection(
    settings_window: &SettingsWindow,
    _window: &mut Window,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let settings = TwgSettings::get_global(cx).clone();
    let original_window = settings_window.original_window;
    let connection = settings_window
        .twg_connection
        .borrow_mut()
        .get_or_insert_with(|| {
            cx.new(|cx| {
                let mut connection = TwgConnection {
                    settings: settings.clone(),
                    original_window,
                    account: None,
                    status: "Not checked. Test the connection or sign in to TWG.".into(),
                    cli_status: None,
                    pending: false,
                    error: false,
                    task: Task::ready(()),
                };
                connection.check(cx);
                connection
            })
        })
        .clone();
    connection.update(cx, |connection, cx| {
        connection.original_window = original_window;
        if connection.settings.executable != settings.executable
            || connection.settings.site != settings.site
        {
            connection.task = Task::ready(());
            connection.settings = settings;
            connection.pending = false;
            connection.account = None;
            connection.check(cx);
        }
    });
    v_flex().px_8().py_4().child(connection).into_any_element()
}

pub(crate) struct TwgConnection {
    settings: TwgSettings,
    original_window: Option<WindowHandle<MultiWorkspace>>,
    account: Option<String>,
    status: String,
    cli_status: Option<String>,
    pending: bool,
    error: bool,
    task: Task<()>,
}

#[derive(Clone, Copy)]
enum AuthOperation {
    Login { force: bool },
    Logout,
}

fn auth_task(settings: &TwgSettings, operation: AuthOperation) -> task::SpawnInTerminal {
    let executable = twg::discover_executable(Some(&settings.executable));
    let (label, mut args) = match operation {
        AuthOperation::Login { force } => {
            let mut args = vec!["login".to_owned()];
            if force {
                args.push("--force".to_owned());
            }
            ("TWG sign in", args)
        }
        AuthOperation::Logout => ("TWG sign out", vec!["logout".to_owned()]),
    };
    if matches!(operation, AuthOperation::Login { .. }) && !settings.site.trim().is_empty() {
        args.extend(["--site".to_owned(), settings.site.trim().to_owned()]);
    }
    task::SpawnInTerminal {
        id: task::TaskId("twg-authentication".to_owned()),
        label: label.to_owned(),
        full_label: label.to_owned(),
        command_label: label.to_owned(),
        // Run the executable directly: configured paths and sites must never be shell source.
        shell: task::Shell::WithArguments {
            program: executable.to_string_lossy().into_owned(),
            args,
            title_override: Some(label.to_owned()),
        },
        use_new_terminal: true,
        reveal: task::RevealStrategy::Always,
        ..Default::default()
    }
}

impl TwgConnection {
    fn check(&mut self, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        self.pending = true;
        self.error = false;
        self.status = "Checking TWG…".into();
        let client = self.settings.client();
        self.task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let version = client.version().await?;
                    let account = client.whoami().await;
                    anyhow::Ok((version, account))
                })
                .await;
            this.update(cx, |this, cx| {
                this.pending = false;
                this.account = None;
                match result {
                    Ok((version, account)) => {
                        this.cli_status = Some(format!("Found · {}", version.trim()));
                        match account {
                            Ok(account) => {
                                this.status = format!("Connected as {}", account.display_name);
                                this.account = Some(account.display_name);
                                this.error = false;
                            }
                            Err(error) => {
                                this.status = format!("Disconnected · {error}");
                                this.error = true;
                            }
                        }
                    }
                    Err(error) => {
                        this.cli_status = Some(
                            "TWG unavailable. Check the executable path or install TWG.".into(),
                        );
                        this.status = format!("Could not run TWG · {error}");
                        this.error = true;
                    }
                }
                cx.notify();
            })
            .log_err();
        });
        cx.notify();
    }

    fn authenticate(&mut self, operation: AuthOperation, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let Some(original_window) = self.original_window else {
            self.status = "Open TWG settings from a workspace to sign in or sign out.".into();
            self.error = true;
            cx.notify();
            return;
        };
        let task = auth_task(&self.settings, operation);
        let result = original_window.update(cx, |multi_workspace, window, cx| {
            window.activate_window();
            multi_workspace
                .workspace()
                .clone()
                .update(cx, |workspace, cx| {
                    workspace.with_local_or_wsl_workspace(
                        window,
                        cx,
                        move |workspace, window, cx| workspace.spawn_in_terminal(task, window, cx),
                    )
                })
        });
        match result {
            Ok(task) => {
                self.pending = true;
                self.error = false;
                self.status = "Complete TWG authentication in the terminal…".into();
                self.task = cx.spawn(async move |this, cx| {
                    let result = match task.await {
                        Ok(task) => task.await,
                        Err(error) => Some(Err(error)),
                    };
                    this.update(cx, |this, cx| {
                        this.pending = false;
                        match result {
                            Some(Ok(status)) if status.success() => {
                                if matches!(operation, AuthOperation::Logout) {
                                    this.account = None;
                                    this.status = "Signed out of TWG.".into();
                                    this.error = false;
                                } else {
                                    this.check(cx);
                                }
                            }
                            Some(Ok(_)) => {
                                this.status = "TWG authentication did not complete. Check the terminal and try again.".into();
                                this.error = true;
                            }
                            Some(Err(error)) => {
                                this.status = format!("Could not run TWG authentication · {error}");
                                this.error = true;
                            }
                            None => {
                                this.status = "The workspace terminal is unavailable.".into();
                                this.error = true;
                            }
                        }
                        cx.notify();
                    }).log_err();
                });
            }
            Err(error) => {
                self.status = format!("Could not open the workspace terminal · {error}");
                self.error = true;
            }
        }
        cx.notify();
    }
}

impl Render for TwgConnection {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let connected = self.account.is_some();
        v_flex()
            .gap_4()
            .child(Label::new("The Work panel reads and edits Jira tickets through the TWG CLI. Zed reuses TWG’s sign-in and stores no tokens itself.").color(Color::Muted))
            .child(v_flex().p_4().gap_2().border_1().border_color(cx.theme().colors().border).rounded_md()
                .child(Label::new(self.status.clone()).color(if self.error { Color::Error } else if connected { Color::Success } else { Color::Muted }))
                .child(Label::new(if self.settings.site.trim().is_empty() { "Site: required for Jira tickets".to_owned() } else { format!("Site: {}", self.settings.site) }).color(Color::Muted))
                .child(h_flex().gap_2()
                    .child(Button::new("twg-test", "Test connection").tab_index(0_isize).disabled(self.pending).on_click(cx.listener(|this, _, _, cx| this.check(cx))))
                    .child(Button::new("twg-connect", if connected { "Reconnect" } else { "Connect" }).tab_index(0_isize).disabled(self.pending).on_click(cx.listener(move |this, _, _, cx| this.authenticate(AuthOperation::Login { force: connected }, cx))))
                    .child(Button::new("twg-sign-out", "Sign out").tab_index(0_isize).disabled(self.pending).on_click(cx.listener(|_, _, window, cx| {
                        let answer = window.prompt(gpui::PromptLevel::Warning, "Sign out of TWG?", Some("TWG has one active login. Signing out affects every tool sharing TWG’s credentials, including this Work panel."), &["Sign out", "Cancel"], cx);
                        cx.spawn(async move |this, cx| {
                            if answer.await == Ok(0) {
                                this.update(cx, |this, cx| this.authenticate(AuthOperation::Logout, cx)).log_err();
                            }
                        }).detach();
                    })))))
            .child(h_flex().gap_2()
                .child(Button::new("twg-check-executable", "Check executable").tab_index(0_isize).disabled(self.pending).on_click(cx.listener(|this, _, _, cx| this.check(cx))))
                .when_some(self.cli_status.clone(), |row, status| row.child(Label::new(status).color(Color::Muted))))
            .child(Label::new("TWG uses one active account. Connect opens TWG’s interactive OAuth sign-in. Credentials are managed by TWG; use TWG’s own configuration tools to inspect them.").size(LabelSize::Small).color(Color::Muted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentication_passes_configured_values_as_arguments() {
        let settings = TwgSettings {
            executable: "/tmp/twg with spaces; echo secret".into(),
            site: "example.atlassian.net; echo secret".into(),
            default_project: String::new(),
            refresh_interval_minutes: 5,
        };
        let login = auth_task(&settings, AuthOperation::Login { force: true });
        assert!(login.command.is_none());
        let task::Shell::WithArguments { program, args, .. } = login.shell else {
            panic!("authentication must run directly")
        };
        assert_eq!(program, settings.executable);
        assert_eq!(args, ["login", "--force", "--site", settings.site.as_str()]);
        let logout = auth_task(&settings, AuthOperation::Logout);
        let task::Shell::WithArguments { args, .. } = logout.shell else {
            panic!("authentication must run directly")
        };
        assert_eq!(args, ["logout"]);
    }
}
