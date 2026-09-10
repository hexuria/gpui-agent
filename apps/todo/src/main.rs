use gpui_kit::component::{ActiveTheme, Root};
use gpui_kit::*;

mod app;

#[cfg(feature = "embedded-host")]
mod agent_bridge;

#[cfg(not(feature = "embedded-host"))]
mod daemon_bridge;

#[cfg(all(feature = "embedded-host", target_os = "macos"))]
mod macos_window;

use app::TodoApp;

fn main() {
    #[cfg(feature = "embedded-host")]
    let mailbox = agent_bridge::maybe_start();

    #[cfg(not(feature = "embedded-host"))]
    daemon_bridge::banner();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);

            #[cfg(feature = "embedded-host")]
            let mailbox = mailbox.clone();

            cx.spawn(async move |cx| {
                let options = WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(96.), px(72.)),
                        size: size(px(520.), px(640.)),
                    })),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Agent Todo".into()),
                        ..Default::default()
                    }),
                    window_min_size: Some(size(px(400.), px(360.))),
                    ..Default::default()
                };

                cx.open_window(options, |window, cx| {
                    let view = cx.new(|cx| {
                        #[cfg(feature = "embedded-host")]
                        {
                            TodoApp::new(window, cx, mailbox.clone())
                        }
                        #[cfg(not(feature = "embedded-host"))]
                        {
                            TodoApp::new(window, cx)
                        }
                    });
                    cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
                })
                .expect("failed to open window");
            })
            .detach();
        });
}
