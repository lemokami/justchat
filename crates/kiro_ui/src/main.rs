//! KiroUI — native GPUI desktop client for kiro-cli over ACP.

mod markdown;
mod theme;
mod workspace;

use gpui::{prelude::*, px, size, App, Bounds, TitlebarOptions, WindowBounds, WindowOptions};
use gpui_platform::application;
use kiro_acp::bridge::{self, BridgeConfig};
use kiro_core::AppState;

use crate::workspace::WorkspaceView;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let settings_path = cwd.join("acp_settings.json");
    let settings = match kiro_acp::Settings::load(&settings_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("{e}; using defaults");
            kiro_acp::Settings::default()
        }
    };

    application().run(move |cx: &mut App| {
        // Start the protocol bridge against the real kiro-cli.
        let config = BridgeConfig::kiro(cwd.clone()).with_settings(settings.clone());
        let bridge = match bridge::start(config.clone()) {
            Ok(handle) => handle,
            Err(e) => {
                tracing::error!("failed to start agent bridge: {e}");
                return;
            }
        };
        let commands = bridge.commands();

        let bounds = Bounds::centered(None, size(px(1100.0), px(760.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("KiroUI".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let state = cx.new(|_| AppState::new(commands));
                cx.new(|cx| WorkspaceView::new(state, config, bridge, window, cx))
            },
        )
        .expect("failed to open window");

        cx.activate(true);
    });
}
