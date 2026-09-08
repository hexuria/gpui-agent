use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, IconName};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use todo_core::TodoStore;

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
    }

    fn build(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("What needs doing?"));

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(&input, window, |this, state, event, window, cx| {
            if matches!(event, InputEvent::Change | InputEvent::PressEnter { .. }) {
                this.store.set_draft(state.read(cx).value().to_string());
            }
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.add_from_input(window, cx);
            }
        }));

        Self {
            store: TodoStore::new(PlatformKind::Desktop),
            _subscriptions: subscriptions,
            #[cfg(feature = "agent")]
            mailbox: None,
            #[cfg(feature = "agent")]
            _refresh: None,
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
        self.store.set_draft(self.input.read(cx).value().to_string());
    }

    #[cfg(feature = "agent")]
        for posted in mailbox.take() {
            self.store
                .set_draft(self.input.read(cx).value().to_string());
            let request = posted.request.clone();
            let response = gpui_agent::handle_request(&mut self.store, request, None);
            let draft = self.store.draft().to_string();
            if draft != self.input.read(cx).value().to_string() {
                self.input.update(cx, |state, cx| {
        };
        for posted in mailbox.take() {
            self.store
                .set_draft(self.input.read(cx).value().to_string());
            let response = gpui_agent::handle_request(&mut self.store, posted.request, None);
            let draft = self.store.draft().to_string();
            if draft != self.input.read(cx).value().to_string() {
                self.input.update(cx, |state, cx| {
                    state.set_value(draft.as_str(), window, cx);
                });
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "agent")]
        self.apply_agent(window, cx);
        #[cfg(not(feature = "agent"))]
        let _ = window;

        let bg = cx.theme().background;
        let fg = cx.theme().foreground;
        let status = match items.len() {
            0 => "No todos".to_string(),
            n => {
                let done = items.iter().filter(|item| item.done).count();
                format!("{n} todos · {done} done")
            }
        };

        v_flex()
            .id("todo-window")
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
                    .child(div().id("todo-input").flex_1().child(Input::new(&self.input)))
                    .child(
                        Button::new("todo-add")
                            .primary()
                            .icon(IconName::Plus)
                            .label("Add")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_from_input(window, cx);
                            })),
                    ),
        #[cfg(feature = "agent")]
        self.apply_agent(window, cx);

        let bg = cx.theme().background;
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted;
        let border = cx.theme().border;
        let items = self.store.items().to_vec();
        let status = match items.len() {
            0 => "No todos".to_string(),
                    .children(if items.is_empty() {
                        vec![
                            div()
        v_flex()
            .id("todo-window")
            .size_full()
            .bg(bg)
            .text_color(fg)
            .px_6()
            .py_5()
            .gap_4()
                        ]
                    } else {
                        items
                            .into_iter()
                            .map(|item| {
                                let toggle_id = SharedString::from(format!("todo-toggle-{}", item.id));
                                let delete_id = SharedString::from(format!("todo-delete-{}", item.id));
                                let row_id = SharedString::from(format!("todo-item-{}", item.id));
                                let done = item.done;
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child("A GPUI Kit 0.6 app with an in-process agent control plane."),
                    ),
            )
                                    .border_1()
                                    .border_color(theme.border)
                                    .items_center()
                                    .child(
                                        Checkbox::new(toggle_id)
                                            .checked(done)
                                            .on_change(cx.listener(move |this, checked, _, cx| {
                                                if this.store.items().iter().any(|t| t.id == item.id && t.done != *checked)
                                                {
                                                    let _ = this.store.toggle(item.id);
                                                    cx.notify();
                                                }
                            })),
                    ),
            )
            .child(self.render_list(items, muted, border, cx))
            .child(
                div()
                    .id("todo-status")
                    .text_sm()
                    .text_color(muted)
                    .child(status),
            )
    }
}

impl TodoApp {
    fn render_list(
        &self,
        items: Vec<todo_core::Todo>,
        muted: Hsla,
        border: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
            .border_1()
            .border_color(border)
            .items_center()
            .child({
                let entity = cx.entity();
                Checkbox::new(toggle_id)
                    .checked(done)
                    .on_click(move |checked, _window, app| {
                        entity.update(app, |this, cx| {
                            if let Some(current) = this.store.items().iter().find(|t| t.id == item_id)
                            {
                                if current.done != *checked {
                                    let _ = this.store.toggle(item_id);
                                    cx.notify();
                                }
                            }
                        });
                    })
            })
            .child(
                div()
                    .flex_1()
        v_flex()
            .id("todo-list")
            .flex_1()
            .gap_2()
                Button::new(delete_id)
                    .ghost()
                    .danger()
                    .label("Delete")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let _ = this.store.delete(item_id);
        muted: Hsla,
        border: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let toggle_id = SharedString::from(format!("todo-toggle-{}", item.id));
        let delete_id = SharedString::from(format!("todo-delete-{}", item.id));
        let row_id = SharedString::from(format!("todo-item-{}", item.id));
        let item_id = item.id;
        let done = item.done;
        let title = item.title.clone();

        h_flex()
            .id(row_id)
            .w_full()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(border)
            .items_center()
            .child(
                Checkbox::new(toggle_id)
                    .label("")
                    .checked(done)
                    .on_change(cx.listener(move |this, checked, _, cx| {
                        if let Some(current) = this.store.items().iter().find(|t| t.id == item_id) {
                            if current.done != *checked {
                                let _ = this.store.toggle(item_id);
                                cx.notify();
                            }
                        }
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .when(done, |el| el.line_through().text_color(muted))
                    .child(title),
            )
            .child(
                Button::new(delete_id)
                    .ghost()
                    .danger()
                    .icon(Icon::new(IconName::Trash).small())
                    .label("Delete")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let _ = this.store.delete(item_id);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}
