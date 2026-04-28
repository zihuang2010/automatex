use crate::constants;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;
use tracing::{error, info, warn};

#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
    pub pub_date: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub available: bool,
    pub info: Option<UpdateInfo>,
}

fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 启动期静默检查：发现新版本则 emit `updater://available`，无新版本只 log。
pub async fn check_for_update_silent(app: AppHandle) {
    match app.updater() {
        Ok(updater) => match updater.check().await {
            Ok(Some(update)) => {
                let info = UpdateInfo {
                    version: update.version.clone(),
                    current_version: current_version(),
                    notes: update.body.clone(),
                    pub_date: update.date.map(|d| d.to_string()),
                };
                info!(version = %info.version, "发现新版本");
                if let Err(e) = app.emit(constants::tauri_event::UPDATER_AVAILABLE, &info) {
                    warn!(error = %e, "派发 updater://available 事件失败");
                }
            },
            Ok(None) => {
                info!(version = %current_version(), "已是最新版本");
            },
            Err(e) => {
                warn!(error = %e, "静默检查更新失败");
            },
        },
        Err(e) => {
            warn!(error = %e, "获取 updater 句柄失败");
        },
    }
}

#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> Result<CheckResult, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(update)) => {
            let info = UpdateInfo {
                version: update.version.clone(),
                current_version: current_version(),
                notes: update.body.clone(),
                pub_date: update.date.map(|d| d.to_string()),
            };
            Ok(CheckResult { available: true, info: Some(info) })
        },
        Ok(None) => Ok(CheckResult { available: false, info: None }),
        Err(e) => Err(format!("检查更新失败: {}", e)),
    }
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| format!("检查更新失败: {}", e))?
        .ok_or_else(|| "已是最新版本".to_string())?;

    let app_for_progress = app.clone();
    let mut downloaded: u64 = 0;
    let download_result = update
        .download_and_install(
            move |chunk_len, content_len| {
                downloaded = downloaded.saturating_add(chunk_len as u64);
                let progress = UpdateProgress { downloaded, total: content_len };
                let _ =
                    app_for_progress.emit(constants::tauri_event::UPDATER_PROGRESS, &progress);
            },
            || {
                info!("更新下载完成，准备安装");
            },
        )
        .await;

    match download_result {
        Ok(_) => {
            info!("更新安装完成，即将重启");
            app.restart();
        },
        Err(e) => {
            let msg = format!("更新安装失败: {}", e);
            error!("{}", msg);
            let _ = app.emit(constants::tauri_event::UPDATER_ERROR, &msg);
            Err(msg)
        },
    }
}
