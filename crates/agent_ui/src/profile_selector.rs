use crate::{CycleModeSelector, ToggleProfileSelector};
use agent_settings::{AgentProfileId, AgentSettings, builtin_profiles};
use fs::Fs;
use gpui::{AnyElement, App, Context, Empty, Entity, FocusHandle, Focusable, Subscription, Window};
use settings::{Settings as _, SettingsStore, ToolPermissionMode, update_settings_file};
use std::sync::Arc;
use ui::{ContextMenu, KeyBinding, LabelSize, PopoverMenu, PopoverMenuHandle, Tooltip, prelude::*};
use zed_actions::OpenSettingsAt;

/// Trait for types that can provide and manage agent profiles
pub trait ProfileProvider {
    /// Get the current profile ID
    fn profile_id(&self, cx: &App) -> AgentProfileId;

    /// Set the profile ID
    fn set_profile(&self, profile_id: AgentProfileId, cx: &mut App);

    /// Check if profiles are supported in the current context (e.g. if the model that is selected has tool support)
    fn profiles_supported(&self, cx: &App) -> bool;

    /// Check if there is a model selected in the current context.
    fn model_selected(&self, cx: &App) -> bool;
}

pub struct ProfileSelector {
    fs: Arc<dyn Fs>,
    provider: Arc<dyn ProfileProvider>,
    approval_menu_handle: PopoverMenuHandle<ContextMenu>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActionApprovalMode {
    Ask,
    Approve,
    FullAccess,
    Custom,
}

impl ActionApprovalMode {
    fn title(self) -> &'static str {
        match self {
            Self::Ask => "Ask for approval",
            Self::Approve => "Approve for me",
            Self::FullAccess => "Full access",
            Self::Custom => "Custom (settings.json)",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Ask => "Always ask before tool actions, network access, and sandbox escapes.",
            Self::Approve => "Only ask for actions detected as potentially unsafe.",
            Self::FullAccess => {
                "Unrestricted access to the internet and any file on your computer."
            }
            Self::Custom => "Uses permissions defined in settings.json.",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Ask => IconName::LockOutlined,
            Self::Approve => IconName::UserCheck,
            Self::FullAccess => IconName::Public,
            Self::Custom => IconName::Settings,
        }
    }
}

const ACTION_APPROVAL_MODES: &[ActionApprovalMode] = &[
    ActionApprovalMode::Ask,
    ActionApprovalMode::Approve,
    ActionApprovalMode::FullAccess,
    ActionApprovalMode::Custom,
];

impl ProfileSelector {
    pub fn new(
        fs: Arc<dyn Fs>,
        provider: Arc<dyn ProfileProvider>,
        focus_handle: FocusHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings_subscription = cx.observe_global::<SettingsStore>(move |_this, cx| {
            cx.notify();
        });
        provider.set_profile(write_profile_id(), cx);

        Self {
            fs,
            provider,
            approval_menu_handle: PopoverMenuHandle::default(),
            focus_handle,
            _subscriptions: vec![settings_subscription],
        }
    }

    pub fn menu_handle(&self) -> PopoverMenuHandle<ContextMenu> {
        self.approval_menu_handle.clone()
    }

    pub fn cycle_profile(&mut self, cx: &mut Context<Self>) {
        if !self.provider.profiles_supported(cx) {
            return;
        }

        self.provider.set_profile(write_profile_id(), cx);
        let current_mode = current_action_approval_mode(cx);
        let current_index = ACTION_APPROVAL_MODES
            .iter()
            .position(|mode| *mode == current_mode)
            .unwrap_or(0);
        if let Some(next_mode) =
            ACTION_APPROVAL_MODES.get((current_index + 1) % ACTION_APPROVAL_MODES.len())
        {
            set_action_approval_mode(self.fs.clone(), *next_mode, cx);
        }
    }
}

impl Focusable for ProfileSelector {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProfileSelector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.provider.model_selected(cx) {
            return Empty.into_any_element();
        }

        if !self.provider.profiles_supported(cx) {
            return Button::new("tools-not-supported-button", "Tools Unsupported")
                .disabled(true)
                .label_size(LabelSize::Small)
                .color(Color::Muted)
                .tooltip(Tooltip::text("This model does not support tools."))
                .into_any_element();
        }

        self.provider.set_profile(write_profile_id(), cx);
        let current_mode = current_action_approval_mode(cx);

        let icon = if self.approval_menu_handle.is_deployed() {
            IconName::ChevronUp
        } else {
            IconName::ChevronDown
        };

        let trigger_button = Button::new("action-approval-selector", current_mode.title())
            .label_size(LabelSize::Small)
            .color(Color::Muted)
            .end_icon(Icon::new(icon).size(IconSize::XSmall).color(Color::Muted));

        let fs = self.fs.clone();
        PopoverMenu::new("action-approval-selector")
            .trigger_with_tooltip(
                trigger_button,
                Tooltip::element(move |_window, cx| {
                    let container = || h_flex().gap_1().justify_between();
                    v_flex()
                        .gap_1()
                        .child(
                            container()
                                .child(Label::new("Change Approval"))
                                .child(KeyBinding::for_action(&ToggleProfileSelector, cx)),
                        )
                        .child(
                            container()
                                .pt_1()
                                .border_t_1()
                                .border_color(cx.theme().colors().border_variant)
                                .child(Label::new("Cycle Approval"))
                                .child(KeyBinding::for_action(&CycleModeSelector, cx)),
                        )
                        .into_any()
                }),
            )
            .menu(move |window, cx| {
                Some(build_action_approval_menu(
                    fs.clone(),
                    current_mode,
                    window,
                    cx,
                ))
            })
            .anchor(gpui::Anchor::BottomRight)
            .with_handle(self.approval_menu_handle.clone())
            .into_any_element()
    }
}

fn write_profile_id() -> AgentProfileId {
    AgentProfileId(builtin_profiles::WRITE.into())
}

fn current_action_approval_mode(cx: &App) -> ActionApprovalMode {
    let settings = AgentSettings::get_global(cx);
    if !settings.tool_permissions.tools.is_empty() {
        return ActionApprovalMode::Custom;
    }

    let sandbox = &settings.sandbox_permissions;
    let restrictive_sandbox = !sandbox.allow_unsandboxed
        && !sandbox.allow_all_hosts
        && !sandbox.allow_fs_write_all
        && sandbox.network_hosts.is_empty()
        && sandbox.write_paths.is_empty();

    match (settings.tool_permissions.default, restrictive_sandbox) {
        (ToolPermissionMode::Confirm, true) => ActionApprovalMode::Ask,
        (ToolPermissionMode::Allow, true) => ActionApprovalMode::Approve,
        (ToolPermissionMode::Allow, false) if sandbox.allow_unsandboxed => {
            ActionApprovalMode::FullAccess
        }
        _ => ActionApprovalMode::Custom,
    }
}

fn build_action_approval_menu(
    fs: Arc<dyn Fs>,
    current_mode: ActionApprovalMode,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ContextMenu> {
    ContextMenu::build(window, cx, move |mut menu, _window, _cx| {
        menu = menu.fixed_width(rems(30.).into());
        for mode in ACTION_APPROVAL_MODES {
            let fs = fs.clone();
            let mode = *mode;
            menu = menu.custom_entry(
                move |_window, cx| render_action_approval_row(mode, mode == current_mode, cx),
                move |window, cx| {
                    if mode == ActionApprovalMode::Custom {
                        window.dispatch_action(
                            Box::new(OpenSettingsAt {
                                path: "agent.tool_permissions".to_string(),
                                target: None,
                            }),
                            cx,
                        );
                    } else {
                        set_action_approval_mode(fs.clone(), mode, cx);
                    }
                },
            );
        }
        menu.key_context("ActionApprovalSelector")
    })
}

fn render_action_approval_row(
    mode: ActionApprovalMode,
    selected: bool,
    _cx: &mut App,
) -> AnyElement {
    h_flex()
        .w_full()
        .min_w_0()
        .min_h(rems_from_px(58.))
        .gap_3()
        .child(
            div().w_6().flex_none().flex().justify_center().child(
                Icon::new(mode.icon())
                    .size(IconSize::Small)
                    .color(Color::Muted),
            ),
        )
        .child(
            v_flex()
                .min_w_0()
                .flex_1()
                .child(Label::new(mode.title()))
                .child(
                    Label::new(mode.description())
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
        )
        .when(selected, |this| {
            this.child(
                Icon::new(IconName::Check)
                    .size(IconSize::Small)
                    .color(Color::Accent),
            )
        })
        .into_any_element()
}

fn set_action_approval_mode(fs: Arc<dyn Fs>, mode: ActionApprovalMode, cx: &mut App) {
    if mode == ActionApprovalMode::Custom {
        return;
    }

    update_settings_file(fs, cx, move |settings, _| {
        let agent = settings.agent.get_or_insert_default();
        agent.default_profile = Some(builtin_profiles::WRITE.into());

        let tool_permissions = agent.tool_permissions.get_or_insert_default();
        tool_permissions.default = Some(match mode {
            ActionApprovalMode::Ask => ToolPermissionMode::Confirm,
            ActionApprovalMode::Approve | ActionApprovalMode::FullAccess => {
                ToolPermissionMode::Allow
            }
            ActionApprovalMode::Custom => return,
        });
        tool_permissions.tools.clear();

        let sandbox_permissions = agent.sandbox_permissions.get_or_insert_default();
        let full_access = mode == ActionApprovalMode::FullAccess;
        sandbox_permissions.allow_unsandboxed = Some(full_access);
        sandbox_permissions.allow_all_hosts = Some(full_access);
        sandbox_permissions.allow_fs_write_all = Some(full_access);
        sandbox_permissions.network_hosts = None;
        sandbox_permissions.write_paths = None;
    });
}
