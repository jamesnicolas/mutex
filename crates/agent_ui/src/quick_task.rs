use std::{collections::VecDeque, rc::Rc};

use agent::SiblingThreadHost;
use editor::{Editor, EditorElement, EditorMode, EditorStyle, MultiBuffer};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    ParentElement, Render, SharedString, Styled, Subscription, Window, relative, rems,
};
use language::Buffer;
use settings::Settings as _;
use ui::{Modal, ModalFooter, ModalHeader, Section, prelude::*};
use workspace::{ModalView, Workspace};

use crate::{
    AgentPanel, NewQuickTask,
    conversation_view::{new_sibling_thread_run_identifier, quick_task_thread_request},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum QuickTaskEligibility {
    Eligible,
    NoGitRepository,
    CollaborativeProject,
    NoAgentPanel,
    NoNativeSiblingHost,
}

impl QuickTaskEligibility {
    fn from_parts(
        project_has_git_repository: bool,
        is_collaborative_project: bool,
        has_agent_panel: bool,
        has_native_sibling_host: bool,
    ) -> Self {
        if !project_has_git_repository {
            Self::NoGitRepository
        } else if is_collaborative_project {
            Self::CollaborativeProject
        } else if !has_agent_panel {
            Self::NoAgentPanel
        } else if !has_native_sibling_host {
            Self::NoNativeSiblingHost
        } else {
            Self::Eligible
        }
    }

    fn is_eligible(&self) -> bool {
        matches!(self, Self::Eligible)
    }

    fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Eligible => None,
            Self::NoGitRepository => {
                Some("Quick Task needs at least one git repository in this project.")
            }
            Self::CollaborativeProject => {
                Some("Quick Task cannot create fresh worktrees in collaborative projects.")
            }
            Self::NoAgentPanel => Some("Quick Task needs the agent panel in this workspace."),
            Self::NoNativeSiblingHost => {
                Some("Quick Task needs native sibling-thread support in this workspace.")
            }
        }
    }
}

pub(crate) fn register(
    workspace: &mut Workspace,
    _window: Option<&mut Window>,
    _cx: &mut Context<Workspace>,
) {
    workspace.register_action(
        |workspace: &mut Workspace,
         _: &NewQuickTask,
         window: &mut Window,
         cx: &mut Context<Workspace>| {
            open(workspace, window, cx);
        },
    );
}

fn open(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let project = workspace.project().clone();
    let (project_name, project_has_git_repository, is_collaborative_project) = {
        let project_ref = project.read(cx);
        (
            project_display_name(project_ref, cx),
            !project_ref.repositories(cx).is_empty(),
            project_ref.is_via_collab(),
        )
    };

    let agent_panel = AgentPanel::for_workspace(workspace, cx);
    let host = agent_panel
        .as_ref()
        .and_then(|panel| AgentPanel::sibling_thread_host_for_panel(panel, window, cx));
    let eligibility = QuickTaskEligibility::from_parts(
        project_has_git_repository,
        is_collaborative_project,
        agent_panel.is_some(),
        host.is_some(),
    );

    workspace.toggle_modal(window, cx, |window, cx| {
        QuickTaskModal::new(project_name, eligibility, host, window, cx)
    });
}

fn project_display_name(project: &project::Project, cx: &App) -> SharedString {
    let mut names = project
        .worktree_root_names(cx)
        .take(3)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    let remaining = project.worktree_root_names(cx).skip(3).count();
    if remaining > 0 {
        names.push(format!("+{remaining} more"));
    }

    if names.is_empty() {
        "empty project".into()
    } else {
        names.join(", ").into()
    }
}

struct QueuedQuickTask {
    request: agent::SiblingThreadRequest,
    title: SharedString,
}

struct QuickTaskQueue {
    host: Rc<dyn SiblingThreadHost>,
    pending_tasks: VecDeque<QueuedQuickTask>,
    is_starting_task: bool,
    active_title: Option<SharedString>,
    status_lines: VecDeque<SharedString>,
}

impl QuickTaskQueue {
    fn new(host: Rc<dyn SiblingThreadHost>) -> Self {
        Self {
            host,
            pending_tasks: VecDeque::new(),
            is_starting_task: false,
            active_title: None,
            status_lines: VecDeque::new(),
        }
    }

    fn enqueue(&mut self, request: agent::SiblingThreadRequest, cx: &mut Context<Self>) {
        let title = request.title.clone();
        self.pending_tasks
            .push_back(QueuedQuickTask { request, title });
        self.start_next(cx);
        cx.notify();
    }

    fn start_next(&mut self, cx: &mut Context<Self>) {
        if self.is_starting_task {
            return;
        }

        let Some(queued_task) = self.pending_tasks.pop_front() else {
            self.active_title = None;
            return;
        };

        self.is_starting_task = true;
        self.active_title = Some(queued_task.title.clone());
        let host = self.host.clone();
        let queue = cx.entity();
        cx.spawn(async move |_, cx| {
            let title = queued_task.title;
            let result = host.create_sibling_thread(queued_task.request, cx).await;
            queue.update(cx, |queue, cx| {
                queue.is_starting_task = false;
                queue.active_title = None;
                match result {
                    Ok(info) => {
                        queue.push_status(format!("Started: {}", info.title).into());
                    }
                    Err(error) => {
                        queue.push_status(format!("Failed: {title}: {error:#}").into());
                    }
                }
                queue.start_next(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn push_status(&mut self, status: SharedString) {
        self.status_lines.push_back(status);
        while self.status_lines.len() > 3 {
            self.status_lines.pop_front();
        }
    }

    fn pending_count(&self) -> usize {
        self.pending_tasks.len()
    }
}

pub(crate) struct QuickTaskModal {
    focus_handle: FocusHandle,
    editor: Entity<Editor>,
    project_name: SharedString,
    eligibility: QuickTaskEligibility,
    queue: Option<Entity<QuickTaskQueue>>,
    last_error: Option<SharedString>,
    _queue_observation: Option<Subscription>,
}

impl QuickTaskModal {
    fn new(
        project_name: SharedString,
        eligibility: QuickTaskEligibility,
        host: Option<Rc<dyn SiblingThreadHost>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let buffer = cx.new(|cx| Buffer::local("", cx));
            let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
            let mut editor = Editor::new(
                EditorMode::AutoHeight {
                    min_lines: 3,
                    max_lines: Some(10),
                },
                buffer,
                None,
                window,
                cx,
            );
            editor.set_placeholder_text("Describe a task...", window, cx);
            editor.set_soft_wrap_mode(language::language_settings::SoftWrap::EditorWidth, cx);
            editor.set_show_indent_guides(false, cx);
            editor.disable_mouse_wheel_zoom();
            editor.set_use_modal_editing(true);
            editor
        });
        let focus_handle = editor.focus_handle(cx);
        focus_handle.focus(window, cx);

        let queue = host.map(|host| cx.new(|_| QuickTaskQueue::new(host)));
        let queue_observation = queue
            .as_ref()
            .map(|queue| cx.observe(queue, |_, _, cx| cx.notify()));

        Self {
            focus_handle,
            editor,
            project_name,
            eligibility,
            queue,
            last_error: None,
            _queue_observation: queue_observation,
        }
    }

    fn submit(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if !self.eligibility.is_eligible() {
            self.last_error = self.eligibility.reason().map(Into::into);
            cx.notify();
            return;
        }

        let Some(queue) = self.queue.clone() else {
            self.last_error = Some("Quick Task needs native sibling-thread support.".into());
            cx.notify();
            return;
        };

        let prompt = self.editor.read(cx).text(cx);
        let prompt = prompt.trim();
        if prompt.is_empty() {
            self.last_error = Some("Enter a task prompt.".into());
            cx.notify();
            return;
        }

        let request = quick_task_thread_request(prompt, &new_sibling_thread_run_identifier());
        self.editor.update(cx, |editor, cx| {
            editor.clear(window, cx);
        });
        self.last_error = None;
        queue.update(cx, |queue, cx| {
            queue.enqueue(request, cx);
        });
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for QuickTaskModal {}

impl Focusable for QuickTaskModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModalView for QuickTaskModal {}

impl Render for QuickTaskModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let queue_state: Option<SharedString> = self.queue.as_ref().and_then(|queue| {
            queue.read(cx).active_title.clone().map_or_else(
                || {
                    let pending_count = queue.read(cx).pending_count();
                    if pending_count > 0 {
                        Some(SharedString::from(format!("{pending_count} queued")))
                    } else {
                        None
                    }
                },
                |title| Some(SharedString::from(format!("Starting: {title}"))),
            )
        });
        let status_lines = self
            .queue
            .as_ref()
            .map(|queue| {
                queue
                    .read(cx)
                    .status_lines
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let settings = theme_settings::ThemeSettings::get_global(cx);
        let text_style = gpui::TextStyle {
            color: cx.theme().colors().text,
            font_family: settings.buffer_font.family.clone(),
            font_fallbacks: settings.buffer_font.fallbacks.clone(),
            font_features: settings.buffer_font.features.clone(),
            font_size: settings.agent_buffer_font_size(cx).into(),
            font_weight: settings.buffer_font.weight,
            line_height: relative(settings.buffer_line_height.value()),
            ..Default::default()
        };

        v_flex()
            .id("quick-task-modal")
            .key_context("QuickTaskComposer")
            .w(rems(36.))
            .elevation_3(cx)
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::submit))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| {
                cx.emit(DismissEvent);
            }))
            .child(
                Modal::new("quick-task", None)
                    .header(
                        ModalHeader::new()
                            .headline("Quick Task")
                            .description(format!(
                                "Project: {}. Tasks run in a fresh worktree.",
                                self.project_name
                            ))
                            .show_dismiss_button(true),
                    )
                    .section(
                        Section::new().child(
                            v_flex()
                                .gap_2()
                                .when(self.eligibility.is_eligible(), |this| {
                                    this.child(
                                        div()
                                            .key_context("Editor")
                                            .w_full()
                                            .min_h(rems(5.))
                                            .max_h(rems(16.))
                                            .rounded_md()
                                            .border_1()
                                            .border_color(cx.theme().colors().border)
                                            .bg(cx.theme().colors().editor_background)
                                            .p_2()
                                            .child(EditorElement::new(
                                                &self.editor,
                                                EditorStyle {
                                                    background: cx
                                                        .theme()
                                                        .colors()
                                                        .editor_background,
                                                    local_player: cx.theme().players().local(),
                                                    text: text_style,
                                                    syntax: cx.theme().syntax().clone(),
                                                    inlay_hints_style:
                                                        editor::make_inlay_hints_style(cx),
                                                    ..Default::default()
                                                },
                                            )),
                                    )
                                })
                                .when(!self.eligibility.is_eligible(), |this| {
                                    this.child(
                                        Label::new(self.eligibility.reason().unwrap_or(""))
                                            .color(Color::Muted)
                                            .size(LabelSize::Small),
                                    )
                                })
                                .when_some(self.last_error.clone(), |this, error| {
                                    this.child(
                                        Label::new(error)
                                            .color(Color::Error)
                                            .size(LabelSize::Small),
                                    )
                                })
                                .children(status_lines.into_iter().map(|status| {
                                    Label::new(status)
                                        .color(Color::Muted)
                                        .size(LabelSize::Small)
                                })),
                        ),
                    )
                    .footer(
                        ModalFooter::new()
                            .when_some(queue_state, |this, status| {
                                this.start_slot(
                                    Label::new(status)
                                        .color(Color::Muted)
                                        .size(LabelSize::Small),
                                )
                            })
                            .end_slot(
                                Button::new("start-quick-task", "Start")
                                    .disabled(!self.eligibility.is_eligible())
                                    .on_click(cx.listener(|this, _event, window, cx| {
                                        this.submit(&menu::Confirm, window, cx);
                                    })),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quick_task_eligibility_requires_git_and_host() {
        assert_eq!(
            QuickTaskEligibility::from_parts(true, false, true, true),
            QuickTaskEligibility::Eligible
        );
        assert_eq!(
            QuickTaskEligibility::from_parts(false, false, true, true),
            QuickTaskEligibility::NoGitRepository
        );
        assert_eq!(
            QuickTaskEligibility::from_parts(true, true, true, true),
            QuickTaskEligibility::CollaborativeProject
        );
        assert_eq!(
            QuickTaskEligibility::from_parts(true, false, false, true),
            QuickTaskEligibility::NoAgentPanel
        );
        assert_eq!(
            QuickTaskEligibility::from_parts(true, false, true, false),
            QuickTaskEligibility::NoNativeSiblingHost
        );
    }
}
