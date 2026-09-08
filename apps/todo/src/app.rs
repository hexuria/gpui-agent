use std::collections::HashMap;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent as FieldEvent, InputState};
use gpui_kit::component::{ActiveTheme, ElementExt, Icon, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use todo_core::{TodoStore, ids};

#[cfg(feature = "agent")]
use gpui_agent::mailbox::AgentMailbox;
use gpui_agent::protocol::PlatformKind;

pub struct TodoApp {
    store: TodoStore,
    input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
    #[cfg(feature = "agent")]
    mailbox: Option<AgentMailbox>,
    #[cfg(feature = "agent")]
    _refresh: Option<Task<()>>,
    #[cfg(feature = "agent")]
    layout_bounds: HashMap<String, gpui_agent::Bounds>,
    #[cfg(feature = "agent")]
    agent_cursor: gpui_agent::AgentCursor,
}

impl TodoApp {
    #[cfg(feature = "agent")]
    pub fn new(window: &mut Window, cx: &mut Context<Self>, mailbox: Option<AgentMailbox>) -> Self {
        let mut app = Self::build(window, cx);
        app.mailbox = mailbox;
        if app.mailbox.is_some() {
            app._refresh = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(16))
                        .await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
        app
    }

    #[cfg(not(feature = "agent"))]
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::build(window, cx)
    }

    fn build(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("What needs doing?"));

        let mut subscriptions = Vec::new();
        subscriptions.push(
            cx.subscribe_in(&input, window, |this, state, event, window, cx| {
                if matches!(event, FieldEvent::Change | FieldEvent::PressEnter { .. }) {
                    this.store.set_draft(state.read(cx).value().to_string());
                }
                if matches!(event, FieldEvent::PressEnter { .. }) {
                    this.add_from_input(window, cx);
                }
            }),
        );

        Self {
            store: TodoStore::new(PlatformKind::Desktop),
            input,
            _subscriptions: subscriptions,
            #[cfg(feature = "agent")]
            mailbox: None,
            #[cfg(feature = "agent")]
            _refresh: None,
            #[cfg(feature = "agent")]
            layout_bounds: HashMap::new(),
            #[cfg(feature = "agent")]
            agent_cursor: gpui_agent::AgentCursor::session_default(),
        }
    }

    fn add_from_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.input.read(cx).value().to_string();
        if self.store.add(title).is_ok() {
            self.input.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
            cx.notify();
        }
    }

    fn sync_draft_from_input(&mut self, cx: &App) {
        self.store
            .set_draft(self.input.read(cx).value().to_string());
    }

    #[cfg(feature = "agent")]
    fn apply_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mailbox) = self.mailbox.clone() else {
            return;
        };
        for posted in mailbox.take() {
            self.sync_draft_from_input(cx);
            self.record_window_bounds(window);

            let shutdown = matches!(posted.request.op, gpui_agent::Op::Shutdown);
            let response = if posted.request.op.is_virtual_input() {
                match self.dispatch_virtual(&posted.request.op, window, cx) {
                    Ok(result) => {
                        let mut resp = gpui_agent::Response::ok(&posted.request.id);
                        resp.result = result.value;
                        resp
                    }
                    Err(error) => gpui_agent::Response::err(&posted.request.id, error),
                }
            } else {
                let mut response =
                    gpui_agent::handle_request(&mut self.store, posted.request.clone(), None);
                if let Some(tree) = response.tree.as_mut() {
                    tree.apply_bounds_map(&self.layout_bounds);
                }
                response
            };

            let draft = self.store.draft().to_string();
            if draft != self.input.read(cx).value().to_string() {
                self.input.update(cx, |state, cx| {
                    state.set_value(draft.as_str(), window, cx);
                });
            }
            posted.reply(response);
            if shutdown {
                cx.quit();
            }
            cx.notify();
        }
    }

    #[cfg(feature = "agent")]
    fn record_window_bounds(&mut self, window: &Window) {
        let bounds = window.bounds();
        self.layout_bounds.insert(
            ids::WINDOW.into(),
            gpui_agent::Bounds {
                x: f32::from(bounds.origin.x),
                y: f32::from(bounds.origin.y),
                w: f32::from(bounds.size.width),
                h: f32::from(bounds.size.height),
            },
        );
    }

    #[cfg(feature = "agent")]
    fn snapshot_with_bounds(&self) -> gpui_agent::UiTree {
        let mut tree = self.store.tree();
        tree.apply_bounds_map(&self.layout_bounds);
        tree
    }

    #[cfg(feature = "agent")]
    fn dispatch_virtual(
        &mut self,
        op: &gpui_agent::Op,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<gpui_agent::DispatchResult, String> {
        match op {
            gpui_agent::Op::Click { target, .. } => self.virtual_click(target, window, cx),
            gpui_agent::Op::Type { target, text, .. } => {
                self.virtual_type(target, text, window, cx)
            }
            gpui_agent::Op::Key { target, key, .. } => self.virtual_key(target, key, window, cx),
            _ => Err(gpui_agent::virtual_unavailable(
                "only click, type, and key support virtual delivery",
            )),
        }
    }

    #[cfg(feature = "agent")]
    fn virtual_click(
        &mut self,
        target: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<gpui_agent::DispatchResult, String> {
        let plan = gpui_agent::plan_click(&self.snapshot_with_bounds(), target)?;
        self.agent_cursor.move_to(plan.x, plan.y);
        self.dispatch_pointer_click(plan.x, plan.y, window, cx);
        Ok(gpui_agent::DispatchResult::json(serde_json::json!({
            "delivery": "virtual",
            "target": target,
            "x": plan.x,
            "y": plan.y,
            "path": "gpui.dispatch_event"
        })))
    }

    #[cfg(feature = "agent")]
    fn virtual_type(
        &mut self,
        target: &str,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<gpui_agent::DispatchResult, String> {
        self.virtual_click(target, window, cx)?;
        if target == ids::INPUT {
            self.input.update(cx, |state, cx| state.focus(window, cx));
        }
        for token in gpui_agent::text_keystrokes(text)? {
            let keystroke = Keystroke::parse(&token).map_err(|err| err.to_string())?;
            window.dispatch_keystroke(keystroke, cx);
        }
        self.sync_draft_from_input(cx);
        Ok(gpui_agent::DispatchResult::json(serde_json::json!({
            "delivery": "virtual",
            "target": target,
            "text": text,
            "path": "gpui.dispatch_keystroke"
        })))
    }

    #[cfg(feature = "agent")]
    fn virtual_key(
        &mut self,
        target: &str,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<gpui_agent::DispatchResult, String> {
        self.virtual_click(target, window, cx)?;
        if target == ids::INPUT {
            self.input.update(cx, |state, cx| state.focus(window, cx));
        }
        let token = gpui_agent::keystroke_token(key)?;
        let keystroke = Keystroke::parse(&token).map_err(|err| err.to_string())?;
        window.dispatch_keystroke(keystroke, cx);
        self.sync_draft_from_input(cx);
        Ok(gpui_agent::DispatchResult::json(serde_json::json!({
            "delivery": "virtual",
            "target": target,
            "key": key,
            "path": "gpui.dispatch_keystroke"
        })))
    }

    /// Inject move + down + up through GPUI's window event pipeline.
    /// Updates GPUI's in-window mouse position only — never the OS cursor.
    #[cfg(feature = "agent")]
    fn dispatch_pointer_click(&self, x: f32, y: f32, window: &mut Window, cx: &mut Context<Self>) {
        let position = point(px(x), px(y));
        let modifiers = Modifiers::default();
        window.dispatch_event(
            MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers,
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers,
                click_count: 1,
            }
            .to_platform_input(),
            cx,
        );
    }

    /// Record a child's painted bounds under a semantic id (previous frame is
    /// what virtual click uses — same timing as real input).
    #[cfg(feature = "agent")]
    fn track_as(
        &self,
        semantic_id: &str,
        cx: &mut Context<Self>,
        child: impl IntoElement,
    ) -> impl IntoElement {
        let entity = cx.entity();
        let record_id = semantic_id.to_string();
        div()
            .id(SharedString::from(format!("{semantic_id}-hit")))
            .child(child)
            .on_prepaint(move |bounds, _window, app| {
                entity.update(app, |this, _| {
                    this.layout_bounds.insert(
                        record_id.clone(),
                        gpui_agent::Bounds {
                            x: f32::from(bounds.origin.x),
                            y: f32::from(bounds.origin.y),
                            w: f32::from(bounds.size.width),
                            h: f32::from(bounds.size.height),
                        },
                    );
                });
            })
    }

    #[cfg(not(feature = "agent"))]
    fn track_as(
        &self,
        _semantic_id: &str,
        _cx: &mut Context<Self>,
        child: impl IntoElement,
    ) -> impl IntoElement {
        child
    }
}

impl Render for TodoApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "agent")]
        self.apply_agent(window, cx);
        #[cfg(not(feature = "agent"))]
        let _ = window;

        let theme = cx.theme().clone();
        let items = self.store.items().to_vec();
        let status = match items.len() {
            0 => "No todos".to_string(),
            n => {
                let done = items.iter().filter(|item| item.done).count();
                format!("{n} todos · {done} done")
            }
        };

        #[cfg(feature = "agent")]
        let cursor = self.agent_cursor.clone();

        v_flex()
            .id("todo-window")
            .relative()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .px_6()
            .py_5()
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Agent Todo"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("A GPUI Kit 0.6 app with an in-process agent control plane."),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .w_full()
                    .child(self.track_as(
                        ids::INPUT,
                        cx,
                        div().id(ids::INPUT).flex_1().child(Input::new(&self.input)),
                    ))
                    .child(
                        self.track_as(
                            ids::ADD,
                            cx,
                            Button::new(ids::ADD)
                                .primary()
                                .icon(IconName::Plus)
                                .label("Add")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.add_from_input(window, cx);
                                })),
                        ),
                    ),
            )
            .child(self.render_list(items, theme.muted_foreground, theme.border, cx))
            .child(
                div()
                    .id(ids::STATUS)
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(status),
            )
            .when(
                {
                    #[cfg(feature = "agent")]
                    {
                        cursor.visible
                    }
                    #[cfg(not(feature = "agent"))]
                    {
                        false
                    }
                },
                |el| {
                    #[cfg(feature = "agent")]
                    {
                        el.child(agent_cursor_overlay(&cursor))
                    }
                    #[cfg(not(feature = "agent"))]
                    {
                        el
                    }
                },
            )
    }
}

#[cfg(feature = "agent")]
fn agent_cursor_overlay(cursor: &gpui_agent::AgentCursor) -> impl IntoElement {
    // Painted overlay only. No hitbox id, no OS pointer warp.
    div()
        .absolute()
        .left(px(cursor.x - 7.0))
        .top(px(cursor.y - 7.0))
        .w(px(14.))
        .h(px(14.))
        .rounded_full()
        .bg(hsla(cursor.hue, 0.85, 0.55, 0.92))
        .border_2()
        .border_color(hsla(0.0, 0.0, 1.0, 0.95))
}

impl TodoApp {
    fn render_list(
        &self,
        items: Vec<todo_core::Todo>,
        muted: Hsla,
        border: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let children: Vec<AnyElement> = if items.is_empty() {
            vec![
                div()
                    .id(ids::EMPTY)
                    .p_4()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child("No todos yet. Add one above."),
                    )
                    .into_any_element(),
            ]
        } else {
            items
                .into_iter()
                .map(|item| self.render_item(item, muted, border, cx))
                .collect()
        };

        v_flex()
            .id(ids::LIST)
            .flex_1()
            .gap_2()
            .w_full()
            .children(children)
    }

    fn render_item(
        &self,
        item: todo_core::Todo,
        muted: Hsla,
        border: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let toggle_id = ids::toggle(item.id);
        let delete_id = ids::delete(item.id);
        let row_id = ids::item(item.id);
        let item_id = item.id;
        let done = item.done;
        let title = item.title.clone();
        let entity = cx.entity();

        let row = h_flex()
            .id(SharedString::from(row_id.clone()))
            .w_full()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(border)
            .items_center()
            .child(
                self.track_as(
                    &toggle_id,
                    cx,
                    Checkbox::new(SharedString::from(toggle_id.clone()))
                        .label("")
                        .checked(done)
                        .on_click({
                            let entity = entity.clone();
                            move |checked, _window, app| {
                                entity.update(app, |this, cx| {
                                    if let Some(current) =
                                        this.store.items().iter().find(|t| t.id == item_id)
                                    {
                                        if current.done != *checked {
                                            let _ = this.store.toggle(item_id);
                                            cx.notify();
                                        }
                                    }
                                });
                            }
                        }),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .when(done, |el| el.line_through().text_color(muted))
                    .child(title),
            )
            .child(
                self.track_as(
                    &delete_id,
                    cx,
                    Button::new(SharedString::from(delete_id.clone()))
                        .ghost()
                        .danger()
                        .icon(Icon::new(IconName::Delete).small())
                        .label("Delete")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let _ = this.store.delete(item_id);
                            cx.notify();
                        })),
                ),
            );

        #[cfg(feature = "agent")]
        {
            self.track_as(&row_id, cx, row).into_any_element()
        }
        #[cfg(not(feature = "agent"))]
        {
            row.into_any_element()
        }
    }
}
