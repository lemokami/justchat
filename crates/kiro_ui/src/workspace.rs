//! The root GPUI view: a horizontal split of [`SidebarView`]-style session list
//! and the chat area, plus the input editor, status bar, and the event pump
//! that drains protocol events into the shared [`AppState`].

use gpui::{
    div, img, prelude::*, px, Context, Entity, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    PathPromptOptions, SharedString, Window,
};
use kiro_acp::bridge::{self, BridgeConfig};
use kiro_acp::BridgeHandle;
use kiro_core::app_state::{ConnectionStatus, Role};
use kiro_core::AppState;

use crate::markdown;
use crate::theme::{self, c};

/// The root view.
pub struct WorkspaceView {
    state: Entity<AppState>,
    focus: FocusHandle,
    // Whether the model-selection dropdown is open.
    show_model_menu: bool,
    // Config used to (re)start the agent bridge.
    config: BridgeConfig,
    // Kept alive for the lifetime of the UI; dropping it shuts down the agent.
    _bridge: BridgeHandle,
}

impl WorkspaceView {
    /// Build the root view, start the event pump, and focus the input.
    pub fn new(
        state: Entity<AppState>,
        config: BridgeConfig,
        mut bridge: BridgeHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);

        // Re-render whenever the shared state changes.
        cx.observe(&state, |_, _, cx| cx.notify()).detach();

        if let Some(events) = bridge.take_events() {
            Self::spawn_pump(state.clone(), events, cx);
        }

        Self {
            state,
            focus,
            show_model_menu: false,
            config,
            _bridge: bridge,
        }
    }

    /// Drain protocol events into the state entity on the GPUI executor.
    fn spawn_pump(
        state: Entity<AppState>,
        mut events: kiro_acp::protocol::EventReceiver,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| loop {
            let mut disconnected = false;
            loop {
                match events.try_recv() {
                    Ok(ev) => {
                        state.update(cx, |s, _| s.apply_event(ev));
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break; // view gone
            }
            if disconnected {
                break;
            }
            cx.background_executor()
                .timer(std::time::Duration::from_millis(8))
                .await;
        })
        .detach();
    }

    /// Restart the agent bridge after a crash, preserving session history.
    fn reconnect(&mut self, cx: &mut Context<Self>) {
        let Ok(mut bridge) = bridge::start(self.config.clone()) else {
            return;
        };
        let commands = bridge.commands();
        if let Some(events) = bridge.take_events() {
            Self::spawn_pump(self.state.clone(), events, cx);
        }
        self.state.update(cx, |s, _| {
            s.set_commands(commands);
            s.mark_reconnecting();
        });
        self._bridge = bridge;
        cx.notify();
    }

    /// Open the native file picker and stage the chosen files as attachments.
    fn open_file_picker(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = this.update(cx, |view, cx| {
                    view.state.update(cx, |s, _| {
                        for p in paths {
                            s.add_attachment(p);
                        }
                    });
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let key = ks.key.as_str();
        let shift = ks.modifiers.shift;
        let cmd = ks.modifiers.platform || ks.modifiers.control;

        self.state.update(cx, |s, _| match key {
            "enter" if shift => s.input.push('\n'),
            "enter" => {
                s.submit_input();
            }
            "backspace" => {
                s.input.pop();
            }
            _ => {
                if !cmd {
                    if let Some(ch) = &ks.key_char {
                        s.input.push_str(ch);
                    }
                }
            }
        });
        cx.notify();
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (label, color, disconnected, model_name, has_models) = {
            let state = self.state.read(cx);
            let (label, color, disconnected) = match &state.connection {
                ConnectionStatus::Connecting => ("Connecting…".to_string(), theme::YELLOW, false),
                ConnectionStatus::Connected { protocol_version } => (
                    format!("Ready · ACP v{protocol_version}"),
                    theme::GREEN,
                    false,
                ),
                ConnectionStatus::Disconnected { message } => {
                    (format!("Disconnected: {message}"), theme::RED, true)
                }
            };
            (
                label,
                color,
                disconnected,
                state.current_model_name(),
                !state.available_models.is_empty(),
            )
        };

        let left = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(div().size(px(8.0)).rounded_full().bg(c(color)))
            .child(label)
            .when(disconnected, |d| {
                d.child(
                    div()
                        .id("reconnect-btn")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(c(theme::SURFACE0))
                        .text_color(c(theme::BLUE))
                        .cursor_pointer()
                        .hover(|d| d.bg(c(theme::SURFACE1)))
                        .child("Reconnect")
                        .on_click(cx.listener(|view, _ev, _window, cx| {
                            view.reconnect(cx);
                        })),
                )
            });

        let model_btn = div()
            .id("model-btn")
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(c(theme::SURFACE0))
            .text_color(c(theme::MAUVE))
            .cursor_pointer()
            .hover(|d| d.bg(c(theme::SURFACE1)))
            .child(SharedString::from(format!(
                "⚙ {} ▾",
                model_name.unwrap_or_else(|| "Model".into())
            )))
            .on_click(cx.listener(|view, _ev, _window, cx| {
                view.show_model_menu = !view.show_model_menu;
                cx.notify();
            }));

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(28.0))
            .px_3()
            .bg(c(theme::CRUST))
            .text_color(c(theme::SUBTEXT))
            .text_xs()
            .child(left)
            .when(has_models, |d| d.child(model_btn))
    }

    fn render_model_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (models, current) = {
            let state = self.state.read(cx);
            (state.available_models.clone(), state.current_model.clone())
        };

        let mut list = div()
            .id("model-menu-list")
            .flex()
            .flex_col()
            .gap_1()
            .p_1()
            .min_w(px(260.0))
            .max_h(px(360.0))
            .overflow_y_scroll()
            .rounded_md()
            .bg(c(theme::SURFACE0))
            .border_1()
            .border_color(c(theme::SURFACE1));

        for m in models {
            let is_current = current.as_deref() == Some(m.id.as_str());
            let id = m.id.clone();
            let desc = m.description.clone().unwrap_or_default();
            list = list.child(
                div()
                    .id(SharedString::from(format!("model-{}", m.id)))
                    .flex()
                    .flex_col()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(is_current, |d| d.bg(c(theme::SURFACE1)))
                    .hover(|d| d.bg(c(theme::SURFACE2)))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_1()
                            .text_sm()
                            .text_color(c(theme::TEXT))
                            .child(SharedString::from(m.name.clone()))
                            .when(is_current, |d| {
                                d.child(div().text_color(c(theme::GREEN)).child("✓"))
                            }),
                    )
                    .when(!desc.is_empty(), |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(c(theme::OVERLAY))
                                .child(SharedString::from(desc)),
                        )
                    })
                    .on_click(cx.listener(move |view, _ev, _window, cx| {
                        view.state.update(cx, |s, _| s.set_model(id.clone()));
                        view.show_model_menu = false;
                        cx.notify();
                    })),
            );
        }

        // Anchored above the status bar, bottom-right.
        div().absolute().bottom(px(34.0)).right(px(8.0)).child(list)
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sessions: Vec<(String, SharedString, bool)> = {
            let state = self.state.read(cx);
            let active = state.active_session_id.clone();
            state
                .sessions
                .iter()
                .map(|s| {
                    (
                        s.id.clone(),
                        SharedString::from(s.title.clone()),
                        Some(&s.id) == active.as_ref(),
                    )
                })
                .collect()
        };

        let mut list = div().flex().flex_col().gap_1().w_full();
        for (id, title, is_active) in sessions {
            let id_for_click = id.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("session-{id}")))
                    .w_full()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .text_color(c(theme::TEXT))
                    .when(is_active, |d| d.bg(c(theme::SURFACE1)))
                    .when(!is_active, |d| d.hover(|d| d.bg(c(theme::SURFACE0))))
                    .cursor_pointer()
                    .child(title)
                    .on_click(cx.listener(move |view, _ev, _window, cx| {
                        view.state
                            .update(cx, |s, _| s.switch_session(&id_for_click));
                        cx.notify();
                    })),
            );
        }

        div()
            .flex()
            .flex_col()
            .w(px(240.0))
            .h_full()
            .bg(c(theme::MANTLE))
            .border_r_1()
            .border_color(c(theme::SURFACE0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(c(theme::MAUVE))
                            .child("Sessions"),
                    )
                    .child(
                        div()
                            .id("new-session")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(c(theme::SURFACE0))
                            .text_xs()
                            .text_color(c(theme::TEXT))
                            .cursor_pointer()
                            .hover(|d| d.bg(c(theme::SURFACE1)))
                            .child("+ New")
                            .on_click(cx.listener(|view, _ev, _window, cx| {
                                view.state.read(cx).request_new_session();
                            })),
                    ),
            )
            .child(
                div()
                    .id("session-list")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .px_2()
                    .child(list),
            )
    }

    fn render_message(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let msg = {
            let state = self.state.read(cx);
            state
                .active_session()
                .and_then(|s| s.messages.get(index))
                .cloned()
        };
        let Some(msg) = msg else {
            return div();
        };

        let (label, label_color, bubble_bg) = match msg.role {
            Role::User => ("You", theme::BLUE, theme::SURFACE0),
            Role::Agent => ("Kiro", theme::MAUVE, theme::MANTLE),
            Role::System => ("System", theme::RED, theme::CRUST),
        };

        let mut bubble = div().flex().flex_col().gap_1().w_full().child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(c(label_color))
                .child(label),
        );

        // Thoughts (muted, only if present).
        if !msg.thoughts.is_empty() {
            bubble = bubble.child(
                div()
                    .text_xs()
                    .text_color(c(theme::OVERLAY))
                    .italic()
                    .child(SharedString::from(msg.thoughts.clone())),
            );
        }

        // User's own text sits above its attachments.
        if msg.role != Role::Agent && !msg.content.is_empty() {
            bubble = bubble.child(
                div()
                    .text_color(c(theme::TEXT))
                    .child(SharedString::from(msg.content.clone())),
            );
        }

        // Attachments (images as thumbnails, other files as chips).
        if !msg.attachments.is_empty() {
            let mut atts = div().flex().flex_row().flex_wrap().gap_2().mt_1();
            for att in &msg.attachments {
                if att.is_image {
                    atts = atts.child(
                        img(att.path.clone())
                            .max_w(px(220.0))
                            .max_h(px(220.0))
                            .rounded_md(),
                    );
                } else {
                    atts = atts.child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(c(theme::CRUST))
                            .text_xs()
                            .text_color(c(theme::TEXT))
                            .child(SharedString::from(format!("📄 {}", att.name))),
                    );
                }
            }
            bubble = bubble.child(atts);
        }

        // Tool calls (and any consent prompt) render before the final answer,
        // matching the flow: think → run tools → respond.
        for (ti, _) in msg.tool_calls.iter().enumerate() {
            bubble = bubble.child(self.render_tool_call(index, ti, cx));
        }

        // Agent's final answer (rendered last, as Markdown).
        if msg.role == Role::Agent && !msg.content.is_empty() {
            bubble = bubble.child(markdown::render(&msg.content));
        }

        // Streaming indicator.
        if msg.streaming && msg.content.is_empty() && msg.tool_calls.is_empty() {
            bubble = bubble.child(
                div()
                    .text_sm()
                    .text_color(c(theme::OVERLAY))
                    .child("Kiro is thinking…"),
            );
        }

        div()
            .flex()
            .flex_col()
            .w_full()
            .p_3()
            .rounded_lg()
            .bg(c(bubble_bg))
            .child(bubble)
    }

    fn render_tool_call(
        &self,
        msg_index: usize,
        tool_index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tc = {
            let state = self.state.read(cx);
            state
                .active_session()
                .and_then(|s| s.messages.get(msg_index))
                .and_then(|m| m.tool_calls.get(tool_index))
                .cloned()
        };
        let Some(tc) = tc else {
            return div();
        };

        let status_color = match tc.status.as_str() {
            "completed" => theme::GREEN,
            "failed" => theme::RED,
            "in_progress" | "pending" => theme::YELLOW,
            _ => theme::SUBTEXT,
        };

        let mut block = div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .mt_1()
            .p_2()
            .rounded_md()
            .bg(c(theme::CRUST))
            .border_1()
            .border_color(c(theme::SURFACE0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(c(theme::TEAL))
                            .child(format!("🔧 {}", tc.kind)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(c(theme::TEXT))
                            .child(SharedString::from(tc.title.clone())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(c(status_color))
                            .child(SharedString::from(tc.status.clone())),
                    ),
            );

        if let Some(output) = &tc.output {
            block = block.child(
                div()
                    .text_xs()
                    .text_color(c(theme::SUBTEXT))
                    .font_family("monospace")
                    .child(SharedString::from(output.clone())),
            );
        }

        // Permission prompt (Task 12).
        if let Some(prompt) = &tc.permission {
            let request_id = prompt.request_id;
            let mut options_row = div().flex().flex_row().gap_2().mt_1();
            for opt in &prompt.options {
                let opt_id = opt.id.clone();
                let is_reject = opt.kind.contains("reject");
                let btn_color = if is_reject { theme::RED } else { theme::GREEN };
                options_row = options_row.child(
                    div()
                        .id(SharedString::from(format!("perm-{request_id}-{}", opt.id)))
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(c(theme::SURFACE0))
                        .text_xs()
                        .text_color(c(btn_color))
                        .cursor_pointer()
                        .hover(|d| d.bg(c(theme::SURFACE1)))
                        .child(SharedString::from(opt.name.clone()))
                        .on_click(cx.listener(move |view, _ev, _window, cx| {
                            view.state.update(cx, |s, _| {
                                s.decide_permission(request_id, Some(opt_id.clone()))
                            });
                            cx.notify();
                        })),
                );
            }
            block = block.child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_xs()
                            .text_color(c(theme::YELLOW))
                            .child("Approval required"),
                    )
                    .child(options_row),
            );
        }

        block
    }

    fn render_chat(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (msg_count, has_session) = {
            let state = self.state.read(cx);
            (
                state
                    .active_session()
                    .map(|s| s.messages.len())
                    .unwrap_or(0),
                state.active_session().is_some(),
            )
        };

        let mut list = div()
            .id("messages")
            .flex()
            .flex_col()
            .gap_3()
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .p_4();

        if !has_session {
            list = list.child(
                div()
                    .text_color(c(theme::OVERLAY))
                    .child("Connecting to kiro-cli…"),
            );
        } else if msg_count == 0 {
            list = list.child(
                div()
                    .text_color(c(theme::OVERLAY))
                    .child("Send a message to start the conversation."),
            );
        } else {
            for i in 0..msg_count {
                list = list.child(self.render_message(i, cx));
            }
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .bg(c(theme::BASE))
            .child(list)
            .child(self.render_input(cx))
    }

    fn render_input(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (input, thinking, attachments) = {
            let state = self.state.read(cx);
            (
                state.input.clone(),
                state.is_thinking(),
                state.pending_attachments.clone(),
            )
        };
        let placeholder = input.is_empty();

        // A thin caret indicating the text-input position.
        let caret = || div().w(px(2.0)).h(px(18.0)).bg(c(theme::BLUE));

        // Field content: placeholder (with leading caret) or text (with trailing caret).
        let field_content = if placeholder {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(caret())
                .child(
                    div()
                        .text_color(c(theme::OVERLAY))
                        .child(SharedString::from(
                            "Type a message — Enter to send, Shift+Enter for newline",
                        )),
                )
        } else {
            div()
                .flex()
                .flex_row()
                .items_center()
                .child(
                    div()
                        .text_color(c(theme::TEXT))
                        .child(SharedString::from(input)),
                )
                .child(caret())
        };

        // Staged-attachment chips row (shown above the input when present).
        let chips = if attachments.is_empty() {
            None
        } else {
            let mut row = div().flex().flex_row().flex_wrap().gap_2().mb_2();
            for (i, att) in attachments.iter().enumerate() {
                let mut chip = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(c(theme::SURFACE0))
                    .text_xs()
                    .text_color(c(theme::TEXT));
                if att.is_image {
                    chip = chip.child(img(att.path.clone()).w(px(28.0)).h(px(28.0)).rounded_md());
                } else {
                    chip = chip.child(div().text_color(c(theme::TEAL)).child("📄"));
                }
                chip = chip.child(SharedString::from(att.name.clone())).child(
                    div()
                        .id(SharedString::from(format!("rm-att-{i}")))
                        .text_color(c(theme::RED))
                        .cursor_pointer()
                        .child("✕")
                        .on_click(cx.listener(move |view, _ev, _window, cx| {
                            view.state.update(cx, |s, _| s.remove_attachment(i));
                            cx.notify();
                        })),
                );
                row = row.child(chip);
            }
            Some(row)
        };

        let attach_btn = div()
            .id("attach-btn")
            .px_3()
            .py_2()
            .rounded_md()
            .bg(c(theme::SURFACE0))
            .text_color(c(theme::TEXT))
            .text_sm()
            .cursor_pointer()
            .hover(|d| d.bg(c(theme::SURFACE1)))
            .child("📎")
            .on_click(cx.listener(|view, _ev, _window, cx| {
                view.open_file_picker(cx);
            }));

        let input_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(attach_btn)
            .child(
                div()
                    .flex_1()
                    .min_h(px(40.0))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(c(theme::BASE))
                    .border_1()
                    .border_color(c(theme::SURFACE1))
                    .child(field_content),
            )
            .when(thinking, |d| {
                d.child(
                    div()
                        .id("stop-btn")
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(c(theme::SURFACE0))
                        .text_color(c(theme::RED))
                        .text_sm()
                        .cursor_pointer()
                        .hover(|d| d.bg(c(theme::SURFACE1)))
                        .child("Stop")
                        .on_click(cx.listener(|view, _ev, _window, cx| {
                            view.state.read(cx).cancel_active();
                            cx.notify();
                        })),
                )
            })
            .when(!thinking, |d| {
                d.child(
                    div()
                        .id("send-btn")
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(c(theme::BLUE))
                        .text_color(c(theme::CRUST))
                        .text_sm()
                        .font_weight(gpui::FontWeight::BOLD)
                        .cursor_pointer()
                        .hover(|d| d.bg(c(theme::TEAL)))
                        .child("Send")
                        .on_click(cx.listener(|view, _ev, _window, cx| {
                            view.state.update(cx, |s, _| s.submit_input());
                            cx.notify();
                        })),
                )
            });

        div()
            .flex()
            .flex_col()
            .p_3()
            .border_t_1()
            .border_color(c(theme::SURFACE0))
            .bg(c(theme::MANTLE))
            .when_some(chips, |d, chips| d.child(chips))
            .child(input_row)
    }
}

impl Focusable for WorkspaceView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("KiroUI")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(c(theme::BASE))
            .text_color(c(theme::TEXT))
            .font_family(".SystemUIFont")
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(self.render_sidebar(cx))
                    .child(self.render_chat(cx)),
            )
            .child(self.render_status_bar(cx))
            .when(self.show_model_menu, |d| {
                d.child(self.render_model_menu(cx))
            })
            // Keep a focus sink so clicks anywhere keep keyboard input working.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _ev, window, cx| {
                    view.focus.focus(window, cx);
                }),
            )
    }
}
