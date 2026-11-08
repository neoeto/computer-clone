use serde::Serialize;
use tauri::State;

use harbor_core::audit::AuditEvent;
use harbor_core::engine::PendingApproval;
use harbor_core::error::HarborError;
use harbor_core::sync::{CreateTaskRequest, TaskDetail, TaskSummary};
use harbor_core::trust::TrustedDevice;
use harbor_core::types::{
    DeviceInfo, DiscoveredSource, Entry, RemoteShare, ShareSummary, SourceInfo,
};
use harbor_core::Engine;

use crate::AppState;

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl From<HarborError> for ApiError {
    fn from(value: HarborError) -> Self {
        Self {
            code: value.code().to_string(),
            message: value.to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AppInfo {
    pub device: DeviceInfo,
    pub version: String,
    pub data_dir: String,
    pub default_destination: String,
    pub os: String,
    pub service_running: bool,
}

#[derive(Debug, Serialize)]
pub struct BrowseResult {
    pub path: String,
    pub entries: Vec<Entry>,
}

fn engine(state: &State<'_, AppState>) -> std::sync::Arc<Engine> {
    state.engine.clone()
}

#[tauri::command]
pub async fn get_source_info(state: State<'_, AppState>) -> Result<SourceInfo, ApiError> {
    Ok(engine(&state).source_info().await)
}

#[tauri::command]
pub async fn start_source(state: State<'_, AppState>) -> Result<SourceInfo, ApiError> {
    engine(&state).start_source().await.map_err(ApiError::from)
}

#[tauri::command]
pub async fn stop_source(state: State<'_, AppState>) -> Result<SourceInfo, ApiError> {
    let engine = engine(&state);
    engine.stop_source().await;
    Ok(engine.source_info().await)
}

#[tauri::command]
pub async fn rotate_code(state: State<'_, AppState>) -> Result<SourceInfo, ApiError> {
    Ok(engine(&state).rotate_code().await)
}

#[tauri::command]
pub async fn refresh_shares(state: State<'_, AppState>) -> Result<SourceInfo, ApiError> {
    let engine = engine(&state);
    engine.refresh_stats().await;
    Ok(engine.source_info().await)
}

#[tauri::command]
pub async fn add_share(path: String, state: State<'_, AppState>) -> Result<ShareSummary, ApiError> {
    engine(&state)
        .add_share(&path)
        .await
        .map_err(ApiError::from)
}

#[tauri::command]
pub async fn remove_share(id: String, state: State<'_, AppState>) -> Result<(), ApiError> {
    engine(&state)
        .remove_share(&id)
        .await
        .map_err(ApiError::from)
}

#[tauri::command]
pub async fn set_share_enabled(
    id: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), ApiError> {
    engine(&state)
        .set_share_enabled(&id, enabled)
        .await
        .map_err(ApiError::from)
}

#[tauri::command]
pub async fn pending_requests(
    state: State<'_, AppState>,
) -> Result<Vec<PendingApproval>, ApiError> {
    Ok(engine(&state).pending_requests().await)
}

#[tauri::command]
pub async fn respond_connection(
    request_id: String,
    allow: bool,
    state: State<'_, AppState>,
) -> Result<(), ApiError> {
    engine(&state)
        .approve(&request_id, allow)
        .await
        .map_err(ApiError::from)
}

#[tauri::command]
pub async fn start_discovery(state: State<'_, AppState>) -> Result<(), ApiError> {
    engine(&state)
        .start_discovery()
        .await
        .map_err(ApiError::from)
}

#[tauri::command]
pub async fn stop_discovery(state: State<'_, AppState>) -> Result<(), ApiError> {
    engine(&state).stop_discovery().await;
    Ok(())
}

#[tauri::command]
pub async fn list_discovered(
    state: State<'_, AppState>,
) -> Result<Vec<DiscoveredSource>, ApiError> {
    Ok(engine(&state).discovered_sources())
}

#[tauri::command]
pub async fn start_connect(
    host: String,
    port: u16,
    code: String,
    expected_fingerprint: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, ApiError> {
    Ok(engine(&state)
        .start_connect(host, port, code, expected_fingerprint)
        .await)
}

#[tauri::command]
pub async fn cancel_connect(
    connect_id: String,
    state: State<'_, AppState>,
) -> Result<(), ApiError> {
    engine(&state).cancel_connect(&connect_id).await;
    Ok(())
}

#[tauri::command]
pub async fn list_remote_shares(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<RemoteShare>, ApiError> {
    engine(&state)
        .remote_list_shares(&session_id)
        .await
        .map_err(ApiError::from)
}

#[tauri::command]
pub async fn browse_remote(
    session_id: String,
    share_id: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<BrowseResult, ApiError> {
    let (path, entries) = engine(&state)
        .remote_browse(&session_id, &share_id, &path)
        .await
        .map_err(ApiError::from)?;
    Ok(BrowseResult { path, entries })
}

#[tauri::command]
pub async fn disconnect_source(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<(), ApiError> {
    engine(&state).disconnect_remote(&session_id).await;
    Ok(())
}

#[tauri::command]
pub async fn set_device_name(
    name: String,
    state: State<'_, AppState>,
) -> Result<DeviceInfo, ApiError> {
    engine(&state)
        .set_device_name(&name)
        .await
        .map_err(ApiError::from)
}

#[tauri::command]
pub async fn create_sync_task(
    request: CreateTaskRequest,
    state: State<'_, AppState>,
) -> Result<TaskDetail, ApiError> {
    engine(&state)
        .create_sync_task(request)
        .await
        .map_err(ApiError::from)
}

#[tauri::command]
pub async fn start_sync_task(
    task_id: String,
    state: State<'_, AppState>,
) -> Result<TaskSummary, ApiError> {
    engine(&state)
        .start_sync_task(&task_id)
        .await
        .map_err(ApiError::from)
}

#[tauri::command]
pub async fn pause_sync_task(
    task_id: String,
    state: State<'_, AppState>,
) -> Result<TaskSummary, ApiError> {
    engine(&state)
        .pause_sync_task(&task_id)
        .await
        .map_err(ApiError::from)
}

#[tauri::command]
pub async fn cancel_sync_task(
    task_id: String,
    state: State<'_, AppState>,
) -> Result<TaskSummary, ApiError> {
    engine(&state)
        .cancel_sync_task(&task_id)
        .await
        .map_err(ApiError::from)
}

#[tauri::command]
pub async fn retry_sync_task(
    task_id: String,
    state: State<'_, AppState>,
) -> Result<TaskSummary, ApiError> {
    engine(&state)
        .retry_sync_task(&task_id)
        .await
        .map_err(ApiError::from)
}

#[tauri::command]
pub async fn remove_sync_task(task_id: String, state: State<'_, AppState>) -> Result<(), ApiError> {
    engine(&state)
        .remove_sync_task(&task_id)
        .await
        .map_err(ApiError::from)
}

#[tauri::command]
pub async fn list_sync_tasks(state: State<'_, AppState>) -> Result<Vec<TaskSummary>, ApiError> {
    Ok(engine(&state).list_sync_tasks().await)
}

#[tauri::command]
pub async fn get_sync_task(
    task_id: String,
    state: State<'_, AppState>,
) -> Result<TaskDetail, ApiError> {
    engine(&state)
        .get_sync_task(&task_id)
        .await
        .map_err(ApiError::from)
}

#[tauri::command]
pub async fn list_trusted_devices(
    state: State<'_, AppState>,
) -> Result<Vec<TrustedDevice>, ApiError> {
    Ok(engine(&state).trusted_devices())
}

#[tauri::command]
pub async fn revoke_trusted_device(
    device_id: String,
    state: State<'_, AppState>,
) -> Result<(), ApiError> {
    engine(&state)
        .revoke_trusted_device(&device_id)
        .map_err(ApiError::from)
}

#[tauri::command]
pub async fn list_audit_events(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<AuditEvent>, ApiError> {
    Ok(engine(&state).audit_events(limit.unwrap_or(50).min(500)))
}

#[tauri::command]
pub async fn open_folder(path: String, state: State<'_, AppState>) -> Result<(), ApiError> {
    let _ = state;
    let target = std::path::PathBuf::from(&path);
    if !target.is_dir() {
        return Err(ApiError {
            code: "not_found".into(),
            message: "文件夹不存在".into(),
        });
    }
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = std::process::Command::new("explorer");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");

    command.arg(&path).spawn().map_err(|err| ApiError {
        code: "internal".into(),
        message: format!("打开文件夹失败：{err}"),
    })?;
    Ok(())
}

#[tauri::command]
pub async fn get_app_info(state: State<'_, AppState>) -> Result<AppInfo, ApiError> {
    let engine = engine(&state);
    let info = engine.source_info().await;
    Ok(AppInfo {
        device: info.device,
        version: harbor_core::APP_VERSION.to_string(),
        data_dir: engine.data_dir().display().to_string(),
        default_destination: engine.default_destination(),
        os: harbor_core::util::os_label(),
        service_running: info.running,
    })
}
