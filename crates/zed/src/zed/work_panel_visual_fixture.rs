use super::*;
use gpui::{AnyView, BorrowAppContext, Context, IntoElement, Render, Window};
use work_panel::{
    WorkPanel,
    visual_fixture::{self, PullRequestScreen, Screen},
};

#[derive(Clone, Copy)]
enum CaptureScreen {
    WorkPanel(Screen),
    Ticket(Screen),
    PullRequest(PullRequestScreen),
}

struct FixtureWindow {
    _workspace: Entity<Workspace>,
    view: AnyView,
    panel: Option<(Entity<WorkPanel>, Screen)>,
    exercise: Option<visual_fixture::TicketTabExercise>,
}

impl Render for FixtureWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.view.clone()
    }
}

pub(super) fn run(
    app_state: Arc<AppState>,
    cx: &mut VisualTestAppContext,
    update_baseline: bool,
) -> Result<()> {
    cx.update(|cx| theme_settings::load_bundled_themes(&theme::ThemeRegistry::global(cx)));
    for (appearance, theme_name) in [("dark", "One Dark"), ("light", "One Light")] {
        cx.update(|cx| -> Result<()> {
            cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.theme.theme = Some(settings::ThemeSelection::Static(
                        settings::ThemeName(theme_name.into()),
                    ));
                    let twg = settings.twg.get_or_insert_default();
                    twg.executable = Some("/fixture/twg-must-not-run".into());
                    twg.site = Some("fixture.invalid".into());
                    twg.refresh_interval_minutes = Some(0);
                });
            });
            let theme = theme::ThemeRegistry::global(cx).get(theme_name)?;
            theme::GlobalTheme::update_theme(cx, theme);
            Ok(())
        })?;
        for width in [420.0, 360.0] {
            for screen in [
                Screen::List,
                Screen::ListError,
                Screen::StatusFiltered,
                Screen::CustomStatus,
            ] {
                capture(
                    app_state.clone(),
                    CaptureScreen::WorkPanel(screen),
                    width,
                    appearance,
                    cx,
                    update_baseline,
                )?;
            }
        }
        for width in [1000.0, 700.0] {
            for screen in [
                Screen::Ticket,
                Screen::Edit,
                Screen::Transition,
                Screen::LogWork,
            ] {
                capture(
                    app_state.clone(),
                    CaptureScreen::Ticket(screen),
                    width,
                    appearance,
                    cx,
                    update_baseline,
                )?;
            }
        }
        for screen in [
            PullRequestScreen::Files,
            PullRequestScreen::Overview,
            PullRequestScreen::Comments,
            PullRequestScreen::Commits,
            PullRequestScreen::Inline,
        ] {
            capture(
                app_state.clone(),
                CaptureScreen::PullRequest(screen),
                1000.0,
                appearance,
                cx,
                update_baseline,
            )?;
        }
        capture_settings(app_state.clone(), appearance, cx, update_baseline)?;
    }
    Ok(())
}

fn capture_settings(
    app_state: Arc<AppState>,
    appearance: &str,
    cx: &mut VisualTestAppContext,
    update_baseline: bool,
) -> Result<()> {
    let project = cx.update(|cx| {
        Project::local(
            app_state.client.clone(),
            app_state.node_runtime.clone(),
            app_state.user_store.clone(),
            app_state.languages.clone(),
            app_state.fs.clone(),
            None,
            project::LocalProjectFlags {
                init_worktree_trust: false,
                ..Default::default()
            },
            cx,
        )
    });
    let workspace_window = cx.update(|cx| {
        cx.open_window(
            WindowOptions {
                focus: false,
                show: false,
                ..Default::default()
            },
            |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(None, project, app_state, window, cx));
                cx.new(|cx| MultiWorkspace::new(workspace, window, cx))
            },
        )
    })?;
    cx.run_until_parked();
    let expected_theme = if appearance == "light" {
        "One Light"
    } else {
        "One Dark"
    };
    anyhow::ensure!(
        cx.read(|cx| theme::GlobalTheme::theme(cx).name.as_ref() == expected_theme),
        "Fixture theme changed while opening its window"
    );
    workspace_window.update(cx, |_, window, cx| {
        window.dispatch_action(
            Box::new(zed_actions::OpenSettingsPage {
                page: "TWG".into(),
                target: None,
            }),
            cx,
        )
    })?;
    cx.run_until_parked();
    let settings_window = cx
        .read(|cx| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<SettingsWindow>())
        })
        .context("TWG settings did not open")?;
    settings_window.update(cx, |_, window, cx| {
        window.simulate_mouse_move(point(px(-1.0), px(-1.0)), cx);
    })?;
    run_visual_test(
        &format!("work_panel_settings_{appearance}"),
        settings_window.into(),
        cx,
        update_baseline,
    )?;
    cx.update_window(settings_window.into(), |_, window, _| {
        window.remove_window()
    })?;
    cx.update_window(workspace_window.into(), |_, window, _| {
        window.remove_window()
    })?;
    cx.run_until_parked();
    Ok(())
}

fn capture(
    app_state: Arc<AppState>,
    screen: CaptureScreen,
    width: f32,
    appearance: &str,
    cx: &mut VisualTestAppContext,
    update_baseline: bool,
) -> Result<()> {
    let project = cx.update(|cx| {
        Project::local(
            app_state.client.clone(),
            app_state.node_runtime.clone(),
            app_state.user_store.clone(),
            app_state.languages.clone(),
            app_state.fs.clone(),
            None,
            project::LocalProjectFlags {
                init_worktree_trust: false,
                ..Default::default()
            },
            cx,
        )
    });
    let fixture_window = cx.update(|cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: size(px(width), px(1100.0)),
                })),
                focus: false,
                show: false,
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let workspace =
                        cx.new(|cx| Workspace::new(None, project, app_state, window, cx));
                    let (view, panel) = match screen {
                        CaptureScreen::WorkPanel(screen) => {
                            let panel = cx.new(|cx| {
                                visual_fixture::create(workspace.downgrade(), screen, window, cx)
                            });
                            (panel.clone().into(), Some((panel, screen)))
                        }
                        CaptureScreen::Ticket(screen) => (
                            visual_fixture::create_ticket(
                                workspace.downgrade(),
                                screen,
                                window,
                                cx,
                            ),
                            None,
                        ),
                        CaptureScreen::PullRequest(screen) => (
                            visual_fixture::create_pull_request(
                                workspace.downgrade(),
                                screen,
                                window,
                                cx,
                            ),
                            None,
                        ),
                    };
                    FixtureWindow {
                        _workspace: workspace,
                        view,
                        panel,
                        exercise: None,
                    }
                })
            },
        )
    })?;
    if matches!(screen, CaptureScreen::WorkPanel(Screen::List))
        && width == 420.0
        && appearance == "dark"
    {
        fixture_window.update(cx, |fixture, window, cx| {
            visual_fixture::exercise_status_filter(fixture._workspace.downgrade(), window, cx)?;
            fixture.exercise = Some(visual_fixture::exercise_ticket_tab(
                &fixture.panel.as_ref().unwrap().0,
                window,
                cx,
            )?);
            anyhow::Ok(())
        })??;
    }
    cx.run_until_parked();
    fixture_window.update(cx, |fixture, _, cx| {
        if let Some((panel, screen)) = &fixture.panel {
            visual_fixture::validate(panel.read(cx), *screen, cx)?;
        }
        if let CaptureScreen::Ticket(screen) = screen {
            visual_fixture::validate_ticket(&fixture.view, screen, cx)?;
        }
        if let Some(exercise) = &fixture.exercise {
            exercise.validate(cx)?;
        }
        anyhow::Ok(())
    })??;
    // Native windows initially inherit the host cursor position, which can change hover styling.
    fixture_window.update(cx, |_, window, cx| {
        window.simulate_mouse_move(point(px(-1.0), px(-1.0)), cx);
    })?;
    let result = run_visual_test(
        &format!(
            "work_panel_{}_{}_{appearance}",
            match screen {
                CaptureScreen::WorkPanel(screen) | CaptureScreen::Ticket(screen) => screen.name(),
                CaptureScreen::PullRequest(screen) => screen.name(),
            },
            width as u32
        ),
        fixture_window.into(),
        cx,
        update_baseline,
    );
    fixture_window.update(cx, |fixture, _, cx| {
        if let Some((panel, screen)) = &fixture.panel {
            visual_fixture::validate(panel.read(cx), *screen, cx)?;
        }
        if let CaptureScreen::Ticket(screen) = screen {
            visual_fixture::validate_ticket(&fixture.view, screen, cx)?;
        }
        if let Some(exercise) = &fixture.exercise {
            exercise.validate(cx)?;
        }
        anyhow::Ok(())
    })??;
    cx.update_window(fixture_window.into(), |_, window, _| window.remove_window())?;
    cx.run_until_parked();
    result?;
    Ok(())
}
