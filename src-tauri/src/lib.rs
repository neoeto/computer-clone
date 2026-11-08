mod commands;

use std::sync::Arc;

use harbor_core::Engine;
use tauri::{Emitter, Manager};
use tracing_subscriber::EnvFilter;

pub struct AppState {
    pub engine: Arc<Engine>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,harbor=debug"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = harbor_core::util::app_data_dir();
            tracing::info!(?data_dir, "启动渡口");
            let engine = Engine::new(data_dir)?;

            let handle = app.handle().clone();
            let mut rx = engine.subscribe();
            tauri::async_runtime::spawn(async move {
                while let Ok(event) = rx.recv().await {
                    if let Err(err) = handle.emit("harbor://event", event) {
                        tracing::debug!(?err, "事件发送失败");
                    }
                }
            });

            app.manage(AppState { engine });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_source_info,
            commands::start_source,
            commands::stop_source,
            commands::rotate_code,
            commands::refresh_shares,
            commands::add_share,
            commands::remove_share,
            commands::set_share_enabled,
            commands::pending_requests,
            commands::respond_connection,
            commands::start_discovery,
            commands::stop_discovery,
            commands::list_discovered,
            commands::start_connect,
            commands::cancel_connect,
            commands::list_remote_shares,
            commands::browse_remote,
            commands::disconnect_source,
            commands::set_device_name,
            commands::create_sync_task,
            commands::start_sync_task,
            commands::pause_sync_task,
            commands::cancel_sync_task,
            commands::retry_sync_task,
            commands::remove_sync_task,
            commands::list_sync_tasks,
            commands::get_sync_task,
            commands::open_folder,
            commands::get_app_info,
        ])
        .run(tauri::generate_context!())
        .expect("运行渡口失败");
}
