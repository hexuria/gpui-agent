use gpui_kit::component::{ActiveTheme, Root};
use gpui_kit::*;

mod app;

#[cfg(feature = "agent")]
mod agent_bridge;

use app::TodoApp;

fn main() {
    #[cfg(feature = "agent")]
    let mailbox = agent_bridge::maybe_start();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);

            #[cfg(feature = "agent")]
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
                        #[cfg(feature = "agent")]
                        {
                            TodoApp::new(window, cx, mailbox.clone())
                        }
                        #[cfg(not(feature = "agent"))]
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
