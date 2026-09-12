#[cfg(feature = "embedded-host")]
use std::collections::{HashMap, VecDeque};

#[cfg(feature = "embedded-host")]
use gpui_kit::component::ElementExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent as FieldEvent, InputState};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
#[cfg(feature = "embedded-host")]
use todo_core::TodoStore;
#[cfg(not(feature = "embedded-host"))]
use todo_core::TodoView;
use todo_core::{Page, ids};

#[cfg(feature = "embedded-host")]
use gpui_agent::AgentHost;
#[cfg(feature = "embedded-host")]
use gpui_agent::mailbox::{AgentMailbox, MailboxRequest};
#[cfg(feature = "embedded-host")]
use gpui_agent::protocol::PlatformKind;

/// In-flight `Op::Keybinding` waiting for GPUI to run the Action handler.
#[cfg(feature = "embedded-host")]
struct PendingKeybindingFire {
    posted: MailboxRequest,
    binding: String,
    confirmed_quit: bool,
}

pub struct TodoApp {
    #[cfg(feature = "embedded-host")]
    store: TodoStore,
    #[cfg(not(feature = "embedded-host"))]
    view: TodoView,
    #[cfg(not(feature = "embedded-host"))]
    bridge: crate::daemon_bridge::DaemonBridge,
    #[cfg(not(feature = "embedded-host"))]
    daemon_error: Option<String>,
    input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
    #[cfg(feature = "embedded-host")]
    mailbox: Option<AgentMailbox>,
    #[cfg(feature = "embedded-host")]
    pending_keybinding: Option<PendingKeybindingFire>,
    #[cfg(feature = "embedded-host")]
    keybinding_backlog: VecDeque<MailboxRequest>,
    #[cfg(feature = "embedded-host")]
    last_keybinding_result: Option<(String, Result<gpui_agent::DispatchResult, String>)>,
    _refresh: Option<Task<()>>,
    #[cfg(feature = "embedded-host")]
    layout_bounds: HashMap<String, gpui_agent::Bounds>,
    #[cfg(feature = "embedded-host")]
    agent_cursor: gpui_agent::AgentCursor,
    scroll: ScrollHandle,
    #[cfg(feature = "embedded-host")]
    scrolled_job: Option<crate::scrolled_shot::ScrolledShotJob>,
}

impl TodoApp {
    #[cfg(feature = "embedded-host")]
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
        Self::register_global_actions(cx.weak_entity(), cx);
        app
    }

    #[cfg(feature = "embedded-host")]
    fn register_global_actions(entity: WeakEntity<Self>, cx: &mut App) {
        App::on_action(cx, {
            let entity = entity.clone();
            move |_: &crate::keybindings::GoSettings, cx| {
                entity
                    .update(cx, |this, cx| {
                        let _ = this.run_keybinding_body(ids::KEY_GO_SETTINGS, None, cx);
                    })
                    .ok();
            }
        });
        App::on_action(cx, {
            let entity = entity.clone();
            move |_: &crate::keybindings::Quit, cx| {
                entity
                    .update(cx, |this, cx| {
                        let _ = this.run_keybinding_body(ids::KEY_QUIT, None, cx);
                        this.quit_after_quit_action(cx);
                    })
                    .ok();
            }
        });
    }

    #[cfg(not(feature = "embedded-host"))]
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut app = Self::build(window, cx);
        app._refresh = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(150))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
        app
    }

    fn build(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("What needs doing?"));

        let mut subscriptions = Vec::new();
        subscriptions.push(
            cx.subscribe_in(&input, window, |this, state, event, window, cx| {
                if matches!(event, FieldEvent::Change | FieldEvent::PressEnter { .. }) {
                    #[cfg(feature = "embedded-host")]
                    this.store.set_draft(state.read(cx).value().to_string());
                    #[cfg(not(feature = "embedded-host"))]
                    {
                        let _ = state;
                    }
                }
                if matches!(event, FieldEvent::PressEnter { .. }) {
                    this.add_from_input(window, cx);
                }
            }),
        );

        Self {
            #[cfg(feature = "embedded-host")]
            store: {
                let mut store = TodoStore::new(PlatformKind::Desktop);
                store.seed_overflow_demo(crate::scrolled_shot::DEMO_OVERFLOW_ROWS);
                store
            },
            #[cfg(not(feature = "embedded-host"))]
            view: TodoView::default(),
            #[cfg(not(feature = "embedded-host"))]
            bridge: crate::daemon_bridge::DaemonBridge::start(),
            #[cfg(not(feature = "embedded-host"))]
            daemon_error: None,
            input,
            _subscriptions: subscriptions,
            #[cfg(feature = "embedded-host")]
            mailbox: None,
            #[cfg(feature = "embedded-host")]
            pending_keybinding: None,
            #[cfg(feature = "embedded-host")]
            keybinding_backlog: VecDeque::new(),
            #[cfg(feature = "embedded-host")]
            last_keybinding_result: None,
            _refresh: None,
            #[cfg(feature = "embedded-host")]
            layout_bounds: HashMap::new(),
            #[cfg(feature = "embedded-host")]
            agent_cursor: gpui_agent::AgentCursor::session_default(),
            scroll: ScrollHandle::new(),
            #[cfg(feature = "embedded-host")]
            scrolled_job: None,
        }
    }

    fn add_from_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.input.read(cx).value().to_string();
        #[cfg(feature = "embedded-host")]
        {
            if self.store.add(title).is_ok() {
                self.input.update(cx, |state, cx| {
                    state.set_value("", window, cx);
                });
                cx.notify();
            }
        }
        #[cfg(not(feature = "embedded-host"))]
        {
            if !title.trim().is_empty() {
                self.bridge.set_value(ids::INPUT, title);
                self.bridge.click(ids::ADD);
                self.input.update(cx, |state, cx| {
                    state.set_value("", window, cx);
                });
                cx.notify();
            }
        }
    }

    fn items(&self) -> Vec<todo_core::Todo> {
        #[cfg(feature = "embedded-host")]
        {
            self.store.items().to_vec()
        }
        #[cfg(not(feature = "embedded-host"))]
        {
            self.view.items.clone()
        }
    }

    fn page(&self) -> Page {
        #[cfg(feature = "embedded-host")]
        {
            self.store.page()
        }
        #[cfg(not(feature = "embedded-host"))]
        {
            self.view.page
        }
    }

    fn confirm_delete(&self) -> bool {
        #[cfg(feature = "embedded-host")]
        {
            self.store.confirm_delete()
        }
        #[cfg(not(feature = "embedded-host"))]
        {
            self.view.confirm_delete
        }
    }

    fn sidebar_open(&self) -> bool {
        #[cfg(feature = "embedded-host")]
        {
            self.store.sidebar_open()
        }
        #[cfg(not(feature = "embedded-host"))]
        {
            self.view.sidebar_open
        }
    }

    fn toggle_sidebar(&mut self) {
        #[cfg(feature = "embedded-host")]
        {
            self.store.toggle_sidebar();
            if !self.store.sidebar_open() {
                self.forget_sidebar_bounds();
            }
        }
        #[cfg(not(feature = "embedded-host"))]
        {
            self.bridge.click(ids::NAV_TOGGLE);
        }
    }

    fn go_page(&mut self, page: Page) {
        #[cfg(feature = "embedded-host")]
        {
            self.store.go(page);
        }
        #[cfg(not(feature = "embedded-host"))]
        {
            let target = match page {
                Page::Todos => ids::NAV_TODOS,
                Page::Settings => ids::NAV_SETTINGS,
            };
            self.bridge.click(target);
        }
    }

    fn toggle_item(&mut self, item_id: u64) {
        #[cfg(feature = "embedded-host")]
        {
            let _ = self.store.toggle(item_id);
        }
        #[cfg(not(feature = "embedded-host"))]
        {
            self.bridge.click(ids::toggle(item_id));
        }
    }

    fn delete_item(&mut self, item_id: u64) {
        #[cfg(feature = "embedded-host")]
        {
            let _ = self.store.delete(item_id);
        }
        #[cfg(not(feature = "embedded-host"))]
        {
            self.bridge.click(ids::delete(item_id));
        }
    }

    fn toggle_confirm_delete(&mut self) {
        #[cfg(feature = "embedded-host")]
        {
            self.store.toggle_confirm_delete();
        }
        #[cfg(not(feature = "embedded-host"))]
        {
            self.bridge.click(ids::SETTINGS_CONFIRM_DELETE);
        }
    }

    #[cfg(not(feature = "embedded-host"))]
    fn apply_daemon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(result) = self.bridge.poll() {
            match result {
                Ok(view) => {
                    self.daemon_error = None;
                    if view.draft != self.input.read(cx).value().to_string()
                        && self.page() == Page::Todos
                    {
                        // Only push daemon draft when an agent typed; skip if it matches
                        // after our own clear. Empty daemon draft after add is applied.
                        if view.draft.is_empty() {
                            self.input.update(cx, |state, cx| {
                                state.set_value("", window, cx);
                            });
                        }
                    }
                    self.view = view;
                }
                Err(err) => self.daemon_error = Some(err),
            }
        }
    }

    #[cfg(feature = "embedded-host")]
    fn sync_draft_from_input(&mut self, cx: &App) {
        self.store
            .set_draft(self.input.read(cx).value().to_string());
    }

    #[cfg(feature = "embedded-host")]
    fn apply_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mailbox) = self.mailbox.clone() else {
            return;
        };
        self.keybinding_backlog.extend(mailbox.take());
        if self.scrolled_job.is_some() {
            self.advance_scrolled_job(window, cx);
            return;
        }
        if self.pending_keybinding.is_some() {
            return;
        }
        while let Some(posted) = self.keybinding_backlog.pop_front() {
            self.sync_draft_from_input(cx);
            self.record_window_bounds(window);

            if matches!(posted.request.op, gpui_agent::Op::Keybinding { .. }) {
                if self.start_keybinding_fire(posted, window, cx) {
                    break;
                }
                continue;
            }

            if let gpui_agent::Op::Screenshot { mode, .. } = &posted.request.op {
                if mode.is_scrolled() {
                    let (path, target, max_height_px) = match &posted.request.op {
                        gpui_agent::Op::Screenshot {
                            path,
                            target,
                            max_height_px,
                            ..
                        } => (path.clone(), target.clone(), *max_height_px),
                        _ => unreachable!(),
                    };
                    self.start_scrolled_job(posted, path, target, max_height_px, window, cx);
                    return;
                }
                let path = match &posted.request.op {
                    gpui_agent::Op::Screenshot { path, .. } => path.clone(),
                    _ => unreachable!(),
                };
                let response = match screenshot_this_window(window, path.as_deref()) {
                    Ok(result) => {
                        let mut resp = gpui_agent::Response::ok(&posted.request.id);
                        resp.result = result.value;
                        resp
                    }
                    Err(error) => gpui_agent::Response::err(&posted.request.id, error),
                };
                posted.reply(response);
                cx.notify();
                continue;
            }

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
            } else if let gpui_agent::Op::Assert { spec } = &posted.request.op {
                let mut tree = self.store.tree();
                tree.apply_bounds_map(&self.layout_bounds);
                match gpui_agent::assert_tree(&tree, spec) {
                    Ok(()) => gpui_agent::Response::ok(&posted.request.id),
                    Err(error) => gpui_agent::Response::err(&posted.request.id, error),
                }
            } else {
                let mut response =
                    gpui_agent::handle_request(&mut self.store, posted.request.clone(), None, None);
                if let Some(tree) = response.tree.as_mut() {
                    tree.apply_bounds_map(&self.layout_bounds);
                }
                response
            };

            self.sync_input_from_store(window, cx);
            posted.reply(response);
            if shutdown {
                cx.quit();
            }
            cx.notify();
        }
    }

    #[cfg(all(feature = "embedded-host", target_os = "macos"))]
    fn scroll_metrics(&self) -> Result<gpui_agent::ScrollMetrics, String> {
        let bounds = self.scroll.bounds();
        let w = f32::from(bounds.size.width);
        let h = f32::from(bounds.size.height);
        if h < 1.0 {
            return Err(gpui_agent::scroll_unavailable(
                "todo-list-scroll viewport is not painted yet",
            ));
        }
        let max_y = f32::from(self.scroll.max_offset().y);
        let offset_y = (-f32::from(self.scroll.offset().y)).max(0.0);
        Ok(gpui_agent::ScrollMetrics {
            viewport: gpui_agent::Bounds {
                x: f32::from(bounds.origin.x),
                y: f32::from(bounds.origin.y),
                w,
                h,
            },
            content_height: h + max_y.max(0.0),
            offset_y,
        })
    }

    #[cfg(feature = "embedded-host")]
    fn set_scroll_offset_y(&self, y: f32) {
        self.scroll.set_offset(point(px(0.0), px(-y)));
    }

    #[cfg(all(feature = "embedded-host", target_os = "macos"))]
    fn window_size(window: &Window) -> (f32, f32) {
        let bounds = window.bounds();
        (f32::from(bounds.size.width), f32::from(bounds.size.height))
    }

    #[cfg(feature = "embedded-host")]
    fn start_scrolled_job(
        &mut self,
        posted: gpui_agent::mailbox::MailboxRequest,
        path: Option<String>,
        target: Option<String>,
        max_height_px: Option<u32>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let spec = gpui_agent::ScreenshotSpec {
            path: path.as_deref(),
            mode: gpui_agent::ScreenshotMode::Scrolled,
            target: target.as_deref(),
            max_height_px,
        };
        if let Err(err) = spec.validate_request() {
            reply_mailbox_err(posted, err);
            return;
        }
        let path = match gpui_agent::require_screenshot_path(spec.path) {
            Ok(path) => path.to_string(),
            Err(err) => {
                reply_mailbox_err(posted, err);
                return;
            }
        };
        if let Err(err) = gpui_agent::confine_screenshot_path(&path) {
            reply_mailbox_err(posted, err);
            return;
        }
        let target = match spec.scrolled_target() {
            Ok(t) => t.to_string(),
            Err(err) => {
                reply_mailbox_err(posted, err);
                return;
            }
        };
        if target != ids::LIST_SCROLL {
            reply_mailbox_err(
                posted,
                gpui_agent::scroll_unavailable(format!(
                    "unknown scroll target `{target}` (want {})",
                    ids::LIST_SCROLL
                )),
            );
            return;
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window;
            let _ = cx;
            reply_mailbox_err(
                posted,
                gpui_agent::screenshot_unavailable(
                    "scrolled screenshot is macOS-only (`screencapture -l` tiles). \
                     This OS has no production GPUI framebuffer export (`Window::render_to_image` is \
                     test-support only). Headless stays screenshot_unavailable.",
                ),
            );
            return;
        }
        #[cfg(target_os = "macos")]
        {
            let window_id = match crate::macos_window::cgwindow_id(window) {
                Ok(id) => id,
                Err(err) => {
                    reply_mailbox_err(posted, err);
                    return;
                }
            };
            let original = self.scroll_metrics().map(|m| m.offset_y).unwrap_or(0.0);
            self.scrolled_job = Some(crate::scrolled_shot::ScrolledShotJob {
                reply: posted,
                dest_client: path,
                target,
                original_offset: original,
                tiles: Vec::new(),
                next: 0,
                captured: Vec::new(),
                metrics: None,
                window_size: Self::window_size(window),
                window_id,
                phase: crate::scrolled_shot::ScrolledPhase::WaitMetrics,
                frames_waited: 0,
                awaiting_paint: false,
            });
            cx.notify();
        }
    }

    #[cfg(feature = "embedded-host")]
    fn restore_scroll_offset(&self, y: f32) {
        self.set_scroll_offset_y(y);
    }

    #[cfg(feature = "embedded-host")]
    fn finish_scrolled_job(
        &mut self,
        job: crate::scrolled_shot::ScrolledShotJob,
        response: gpui_agent::Response,
        cx: &mut Context<Self>,
    ) {
        self.restore_scroll_offset(job.original_offset);
        job.reply.reply(response);
        cx.notify();
    }

    #[cfg(feature = "embedded-host")]
    fn advance_scrolled_job(&mut self, window: &Window, cx: &mut Context<Self>) {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window;
            if let Some(job) = self.scrolled_job.take() {
                let id = job.reply.request.id.clone();
                self.finish_scrolled_job(
                    job,
                    gpui_agent::Response::err(
                        id,
                        gpui_agent::screenshot_unavailable("scrolled screenshot is macOS-only"),
                    ),
                    cx,
                );
            }
            return;
        }
        #[cfg(target_os = "macos")]
        {
            self.advance_scrolled_job_macos(window, cx);
        }
    }

    #[cfg(all(feature = "embedded-host", target_os = "macos"))]
    fn advance_scrolled_job_macos(&mut self, window: &Window, cx: &mut Context<Self>) {
        use gpui_agent::{
            MAX_SCROLLED_PNG_BYTES, capture_window_png_bytes, confine_screenshot_path,
            crop_window_png, encode_png_rgba, plan_scroll_tiles, scrolled_dispatch_result,
            stitch_tiles_vertically,
        };

        let mut job = match self.scrolled_job.take() {
            Some(job) => job,
            None => return,
        };
        job.frames_waited += 1;
        job.window_size = Self::window_size(window);

        match job.phase {
            crate::scrolled_shot::ScrolledPhase::WaitMetrics => match self.scroll_metrics() {
                Ok(metrics) => {
                    let cap = match &job.reply.request.op {
                        gpui_agent::Op::Screenshot { max_height_px, .. } => {
                            max_height_px.unwrap_or(gpui_agent::DEFAULT_MAX_HEIGHT_PX)
                        }
                        _ => gpui_agent::DEFAULT_MAX_HEIGHT_PX,
                    };
                    let tiles = match plan_scroll_tiles(&metrics, cap) {
                        Ok(tiles) => tiles,
                        Err(err) => {
                            let id = job.reply.request.id.clone();
                            self.finish_scrolled_job(job, gpui_agent::Response::err(id, err), cx);
                            return;
                        }
                    };
                    job.original_offset = metrics.offset_y;
                    job.metrics = Some(metrics);
                    job.tiles = tiles;
                    job.next = 0;
                    if job.tiles.is_empty() {
                        let id = job.reply.request.id.clone();
                        self.finish_scrolled_job(
                            job,
                            gpui_agent::Response::err(id, "scrolled screenshot produced no tiles"),
                            cx,
                        );
                        return;
                    }
                    self.set_scroll_offset_y(job.tiles[0].offset_y);
                    job.phase = crate::scrolled_shot::ScrolledPhase::WaitPaint;
                    job.awaiting_paint = true;
                    self.scrolled_job = Some(job);
                    cx.notify();
                }
                Err(_) if job.frames_waited < crate::scrolled_shot::METRICS_WAIT_FRAMES => {
                    self.scrolled_job = Some(job);
                    cx.notify();
                }
                Err(err) => {
                    let id = job.reply.request.id.clone();
                    self.finish_scrolled_job(job, gpui_agent::Response::err(id, err), cx);
                }
            },
            crate::scrolled_shot::ScrolledPhase::WaitPaint => {
                if job.awaiting_paint {
                    job.awaiting_paint = false;
                    self.scrolled_job = Some(job);
                    cx.notify();
                    return;
                }
                let spec = job.tiles[job.next];
                let metrics = match job.metrics {
                    Some(m) => m,
                    None => {
                        let id = job.reply.request.id.clone();
                        self.finish_scrolled_job(
                            job,
                            gpui_agent::Response::err(
                                id,
                                gpui_agent::scroll_unavailable("missing scroll metrics"),
                            ),
                            cx,
                        );
                        return;
                    }
                };
                let png = match capture_window_png_bytes(job.window_id) {
                    Ok(png) => png,
                    Err(err) => {
                        let id = job.reply.request.id.clone();
                        self.finish_scrolled_job(job, gpui_agent::Response::err(id, err), cx);
                        return;
                    }
                };
                let slice = match crop_window_png(
                    &png,
                    job.window_size.0,
                    job.window_size.1,
                    metrics.viewport,
                    spec.skip_top_px,
                    spec.take_height_px,
                ) {
                    Ok(slice) => slice,
                    Err(err) => {
                        let id = job.reply.request.id.clone();
                        self.finish_scrolled_job(job, gpui_agent::Response::err(id, err), cx);
                        return;
                    }
                };
                job.captured.push(slice);
                job.next += 1;
                if job.next >= job.tiles.len() {
                    let dest_client = job.dest_client.clone();
                    let target = job.target.clone();
                    let content_h = metrics.content_height.max(metrics.viewport.h);
                    let vh = metrics.viewport.h;
                    let tile_count = job.tiles.len();
                    let stitched = match stitch_tiles_vertically(&job.captured) {
                        Ok(img) => img,
                        Err(err) => {
                            let id = job.reply.request.id.clone();
                            self.finish_scrolled_job(job, gpui_agent::Response::err(id, err), cx);
                            return;
                        }
                    };
                    let bytes = match encode_png_rgba(&stitched) {
                        Ok(bytes) => bytes,
                        Err(err) => {
                            let id = job.reply.request.id.clone();
                            self.finish_scrolled_job(job, gpui_agent::Response::err(id, err), cx);
                            return;
                        }
                    };
                    if bytes.len() > MAX_SCROLLED_PNG_BYTES {
                        let id = job.reply.request.id.clone();
                        self.finish_scrolled_job(
                            job,
                            gpui_agent::Response::err(
                                id,
                                format!(
                                    "stitched png exceeds {MAX_SCROLLED_PNG_BYTES} bytes ({})",
                                    bytes.len()
                                ),
                            ),
                            cx,
                        );
                        return;
                    }
                    let dest = match confine_screenshot_path(&dest_client) {
                        Ok(dest) => dest,
                        Err(err) => {
                            let id = job.reply.request.id.clone();
                            self.finish_scrolled_job(job, gpui_agent::Response::err(id, err), cx);
                            return;
                        }
                    };
                    if let Err(err) = gpui_agent::atomic_write_png(&dest, &bytes) {
                        let id = job.reply.request.id.clone();
                        self.finish_scrolled_job(job, gpui_agent::Response::err(id, err), cx);
                        return;
                    }
                    let dest_str = dest.to_string_lossy().into_owned();
                    let result =
                        scrolled_dispatch_result(&dest_str, &target, content_h, vh, tile_count);
                    let id = job.reply.request.id.clone();
                    let mut resp = gpui_agent::Response::ok(id);
                    resp.result = result.value;
                    self.finish_scrolled_job(job, resp, cx);
                    return;
                }
                self.set_scroll_offset_y(job.tiles[job.next].offset_y);
                job.awaiting_paint = true;
                self.scrolled_job = Some(job);
                cx.notify();
            }
        }
    }

    #[cfg(feature = "embedded-host")]
    fn sync_input_from_store(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.store.draft().to_string();
        if draft != self.input.read(cx).value().to_string() {
            self.input.update(cx, |state, cx| {
                state.set_value(draft.as_str(), window, cx);
            });
        }
    }

    #[cfg(feature = "embedded-host")]
    fn forget_sidebar_bounds(&mut self) {
        self.layout_bounds.remove(ids::NAV);
        self.layout_bounds.remove(ids::NAV_TODOS);
        self.layout_bounds.remove(ids::NAV_SETTINGS);
    }

    #[cfg(feature = "embedded-host")]
    fn record_window_bounds(&mut self, window: &Window) {
        // Painted window clip in the same space as `on_prepaint` widget
        // bounds (local origin). Screen origin is not a clip.
        let bounds = window.bounds();
        self.layout_bounds.insert(
            ids::WINDOW.into(),
            gpui_agent::Bounds {
                x: 0.0,
                y: 0.0,
                w: f32::from(bounds.size.width),
                h: f32::from(bounds.size.height),
            },
        );
        if !self.store.sidebar_open() {
            self.forget_sidebar_bounds();
        }
    }

    #[cfg(feature = "embedded-host")]
    fn snapshot_with_bounds(&self) -> gpui_agent::UiTree {
        let mut tree = self.store.tree();
        tree.apply_bounds_map(&self.layout_bounds);
        tree
    }

    #[cfg(feature = "embedded-host")]
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

    #[cfg(feature = "embedded-host")]
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

    #[cfg(feature = "embedded-host")]
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

    #[cfg(feature = "embedded-host")]
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

    /// Authorize + dispatch the GPUI Action only. Listeners own store mutation.
    /// Returns true when the mailbox reply is deferred until after the handler.
    #[cfg(feature = "embedded-host")]
    fn start_keybinding_fire(
        &mut self,
        posted: MailboxRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let confirmed_quit = gpui_agent::op_is_confirmed_quit(&posted.request.op);
        let catalog = self.store.keybindings();
        let focused = window.is_window_active();
        let entry = match gpui_agent::authorize_keybinding_op(&posted.request.op, &catalog, focused)
        {
            Ok(entry) => entry.clone(),
            Err(error) => {
                let id = posted.request.id.clone();
                posted.reply(gpui_agent::Response::err(id, error));
                self.sync_input_from_store(window, cx);
                cx.notify();
                return false;
            }
        };
        let activate = match &posted.request.op {
            gpui_agent::Op::Keybinding { activate, .. } => *activate,
            _ => false,
        };
        let Some(action) = crate::keybindings::action_for_binding(&entry.id) else {
            let id = posted.request.id.clone();
            posted.reply(gpui_agent::Response::err(
                id,
                format!("unknown binding `{}`", entry.id),
            ));
            self.sync_input_from_store(window, cx);
            cx.notify();
            return false;
        };
        if activate {
            window.activate_window();
        }

        // Record pending *before* dispatch so a sync global listener can see it.
        // Window::dispatch_action always `cx.defer`s; finish is queued after that.
        self.last_keybinding_result = None;
        self.pending_keybinding = Some(PendingKeybindingFire {
            posted,
            binding: entry.id.clone(),
            confirmed_quit,
        });
        match entry.scope {
            gpui_agent::KeybindingScope::Focused => {
                window.dispatch_action(action, cx);
            }
            gpui_agent::KeybindingScope::Global => {
                if focused {
                    window.dispatch_action(action, cx);
                } else {
                    // Kit pin: App::dispatch_action uses the global table when
                    // there is no OS-active window. Do not activate to fake it.
                    cx.dispatch_action(action.as_ref());
                }
            }
        }
        cx.defer_in(window, |this, window, cx| {
            this.finish_keybinding_fire(window, cx);
        });
        true
    }

    #[cfg(feature = "embedded-host")]
    fn finish_keybinding_fire(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_keybinding.take() else {
            return;
        };
        let listener = match self.last_keybinding_result.take() {
            Some((id, result)) if id == pending.binding => Some(result),
            _ => None,
        };
        let response = match gpui_agent::complete_keybinding_action(listener) {
            Ok(result) => {
                let mut resp = gpui_agent::Response::ok(&pending.posted.request.id);
                resp.result = result.value;
                resp
            }
            Err(error) => gpui_agent::Response::err(&pending.posted.request.id, error),
        };
        self.sync_input_from_store(window, cx);
        let confirmed_quit = pending.confirmed_quit;
        pending.posted.reply(response);
        if confirmed_quit && self.store.wants_shutdown() {
            cx.quit();
        }
        cx.notify();
    }

    /// Action body used by keymap / `on_action` listeners. Not the intercept.
    #[cfg(feature = "embedded-host")]
    fn run_keybinding_body(
        &mut self,
        binding: &str,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> Result<gpui_agent::DispatchResult, String> {
        let result = self.store.perform_keybinding(binding);
        self.last_keybinding_result = Some((binding.to_string(), result.clone()));
        let result = result?;
        if binding == ids::KEY_FOCUS_INPUT {
            if let Some(window) = window {
                self.input.update(cx, |state, cx| state.focus(window, cx));
            }
        }
        cx.notify();
        Ok(result)
    }

    #[cfg(feature = "embedded-host")]
    fn perform_keybinding_action(
        &mut self,
        binding: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<gpui_agent::DispatchResult, String> {
        self.run_keybinding_body(binding, Some(window), cx)
    }

    /// Human `cmd-q` must still exit when the agent mailbox is live. Confirmed
    /// agent quit replies first (`finish_keybinding_fire`), then quits.
    #[cfg(feature = "embedded-host")]
    fn quit_after_quit_action(&mut self, cx: &mut Context<Self>) {
        if self.mailbox.is_none() {
            cx.quit();
            return;
        }
        let agent_confirmed_quit = self
            .pending_keybinding
            .as_ref()
            .is_some_and(|pending| pending.confirmed_quit && pending.binding == ids::KEY_QUIT);
        if agent_confirmed_quit {
            return;
        }
        let entity = cx.weak_entity();
        cx.defer(move |cx| {
            let should_quit = entity
                .update(cx, |this, _| {
                    this.store.wants_shutdown() && this.pending_keybinding.is_none()
                })
                .unwrap_or(true);
            if should_quit {
                cx.quit();
            }
        });
    }

    /// Inject move + down + up through GPUI's window event pipeline.
    /// Updates GPUI's in-window mouse position only — never the OS cursor.
    #[cfg(feature = "embedded-host")]
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
    #[cfg(feature = "embedded-host")]
    fn track_as(
        &self,
        semantic_id: &str,
        cx: &mut Context<Self>,
        child: impl IntoElement,
    ) -> impl IntoElement {
        let entity = cx.entity();
        let record_id = semantic_id.to_string();
        // No element id on the tracker — a stateful wrapper would steal hit-tests
        // from the real widget that virtual clicks must reach.
        div().child(child).on_prepaint(move |bounds, _window, app| {
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

    #[cfg(not(feature = "embedded-host"))]
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
        #[cfg(feature = "embedded-host")]
        self.apply_agent(window, cx);
        #[cfg(not(feature = "embedded-host"))]
        self.apply_daemon(window, cx);

        let theme = cx.theme().clone();
        let items = self.items();
        let page = self.page();
        let sidebar_open = self.sidebar_open();
        let status = match items.len() {
            0 => "No todos".to_string(),
            n => {
                let done = items.iter().filter(|item| item.done).count();
                format!("{n} todos · {done} done")
            }
        };

        #[cfg(feature = "embedded-host")]
        let cursor = self.agent_cursor.clone();

        v_flex()
            .id("todo-window")
            .key_context("todo")
            .relative()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .px_6()
            .py_5()
            .gap_4()
            .when(cfg!(feature = "embedded-host"), |el| {
                #[cfg(feature = "embedded-host")]
                {
                    el.on_action(cx.listener(
                        |this, _: &crate::keybindings::FocusInput, window, cx| {
                            let _ =
                                this.perform_keybinding_action(ids::KEY_FOCUS_INPUT, window, cx);
                        },
                    ))
                    .on_action(cx.listener(
                        |this, _: &crate::keybindings::GoSettings, window, cx| {
                            let _ =
                                this.perform_keybinding_action(ids::KEY_GO_SETTINGS, window, cx);
                        },
                    ))
                    .on_action(cx.listener(
                        |this, _: &crate::keybindings::Quit, window, cx| {
                            let _ = this.perform_keybinding_action(ids::KEY_QUIT, window, cx);
                            this.quit_after_quit_action(cx);
                        },
                    ))
                }
                #[cfg(not(feature = "embedded-host"))]
                {
                    el
                }
            })
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Agent Todo"),
                    )
                    .child(div().text_sm().text_color(theme.muted_foreground).child(
                        if cfg!(feature = "embedded-host") {
                            "In-process AgentHost (widget E2E). Product SoT is the daemon."
                        } else {
                            "GUI client of todo-headless (ADR-001). Start the daemon to mutate."
                        },
                    )),
            )
            .when(
                {
                    #[cfg(not(feature = "embedded-host"))]
                    {
                        self.daemon_error.is_some()
                    }
                    #[cfg(feature = "embedded-host")]
                    {
                        false
                    }
                },
                |el| {
                    #[cfg(not(feature = "embedded-host"))]
                    {
                        let err = self
                            .daemon_error
                            .clone()
                            .unwrap_or_else(|| "daemon unavailable".into());
                        el.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(format!("daemon: {err}")),
                        )
                    }
                    #[cfg(feature = "embedded-host")]
                    {
                        el
                    }
                },
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        self.track_as(
                            ids::NAV_TOGGLE,
                            cx,
                            Button::new(ids::NAV_TOGGLE)
                                .ghost()
                                .label(if sidebar_open {
                                    "Hide sidebar"
                                } else {
                                    "Show sidebar"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.toggle_sidebar();
                                    cx.notify();
                                })),
                        ),
                    )
                    .when(sidebar_open, |el| {
                        el.child(
                            self.track_as(
                                ids::NAV,
                                cx,
                                h_flex()
                                    .id(ids::NAV)
                                    .gap_2()
                                    .child(
                                        Button::new(ids::NAV_TODOS)
                                            .when(page == Page::Todos, |b| b.primary())
                                            .when(page != Page::Todos, |b| b.ghost())
                                            .label("Todos")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.go_page(Page::Todos);
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        Button::new(ids::NAV_SETTINGS)
                                            .when(page == Page::Settings, |b| b.primary())
                                            .when(page != Page::Settings, |b| b.ghost())
                                            .label("Settings")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.go_page(Page::Settings);
                                                cx.notify();
                                            })),
                                    ),
                            ),
                        )
                    }),
            )
            .when(page == Page::Todos, |el| {
                el.child(
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
            })
            .when(page == Page::Settings, |el| {
                let confirm = self.confirm_delete();
                el.child(
                    self.track_as(
                        ids::SETTINGS_CONFIRM_DELETE,
                        cx,
                        Checkbox::new(ids::SETTINGS_CONFIRM_DELETE)
                            .label("Confirm before delete")
                            .checked(confirm)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_confirm_delete();
                                cx.notify();
                            })),
                    ),
                )
            })
            .when(
                {
                    #[cfg(feature = "embedded-host")]
                    {
                        cursor.visible
                    }
                    #[cfg(not(feature = "embedded-host"))]
                    {
                        false
                    }
                },
                |el| {
                    #[cfg(feature = "embedded-host")]
                    {
                        el.child(agent_cursor_overlay(&cursor))
                    }
                    #[cfg(not(feature = "embedded-host"))]
                    {
                        el
                    }
                },
            )
    }
}

#[cfg(feature = "embedded-host")]
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

        let list = v_flex()
            .id(ids::LIST)
            .gap_2()
            .w_full()
            .flex_shrink_0()
            .children(children);

        div()
            .id(ids::LIST_SCROLL)
            .flex_1()
            .min_h(px(0.))
            .w_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(list)
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
            .flex_shrink_0()
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
                                    let current_done = this
                                        .items()
                                        .iter()
                                        .find(|t| t.id == item_id)
                                        .map(|t| t.done)
                                        .unwrap_or(false);
                                    if current_done != *checked {
                                        this.toggle_item(item_id);
                                        cx.notify();
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
                            this.delete_item(item_id);
                            cx.notify();
                        })),
                ),
            );

        #[cfg(feature = "embedded-host")]
        {
            self.track_as(&row_id, cx, row).into_any_element()
        }
        #[cfg(not(feature = "embedded-host"))]
        {
            row.into_any_element()
        }
    }
}

#[cfg(feature = "embedded-host")]
fn reply_mailbox_err(posted: gpui_agent::mailbox::MailboxRequest, err: impl Into<String>) {
    let id = posted.request.id.clone();
    posted.reply(gpui_agent::Response::err(id, err));
}

#[cfg(feature = "embedded-host")]
fn screenshot_this_window(
    window: &Window,
    path: Option<&str>,
) -> Result<gpui_agent::DispatchResult, String> {
    let path = gpui_agent::require_screenshot_path(path)?;
    let _dest = gpui_agent::confine_screenshot_path(path)?;
    #[cfg(target_os = "macos")]
    {
        let id = crate::macos_window::cgwindow_id(window)?;
        gpui_agent::capture_window_via_screencapture(id, Some(path))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        let _ = path;
        Err(gpui_agent::screenshot_unavailable(
            "desktop PNG of the app window is macOS-only (`screencapture -l` of this window). \
             This OS has no production GPUI framebuffer export (`Window::render_to_image` is \
             test-support only). Headless stays screenshot_unavailable.",
        ))
    }
}
