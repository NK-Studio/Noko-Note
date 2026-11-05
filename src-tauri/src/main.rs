//! Noko-Note 데스크톱 앱(Tauri 2, Mac·Windows).
//!
//! 앱 안에서 Noko-Note 로컬 서버를 빈 포트로 띄우고, 창에 그 화면을 연다. 웹 버전과
//! 화면·기능이 같고, md 저장(저장 대화상자)과 파일 위치 열기만 데스크톱 기능으로 더한다.

// Windows 릴리스 빌드에서 콘솔 창이 뜨지 않게 한다.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use tauri::{AppHandle, Manager, State, Theme, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

struct AppNoko(noko_note::Noko);

/// 요약을 md 파일로 저장한다. 사용자가 저장 대화상자를 닫으면 `None`.
#[tauri::command]
async fn save_markdown(
    app: AppHandle,
    noko: State<'_, AppNoko>,
    recording_id: i64,
) -> Result<Option<String>, String> {
    let (file_name, body) = noko.0.markdown(recording_id).map_err(|e| format!("{e:#}"))?;
    let picked = app
        .dialog()
        .file()
        .set_file_name(&file_name)
        .add_filter("Markdown", &["md"])
        .blocking_save_file();
    let Some(picked) = picked else { return Ok(None) };
    let path = picked.into_path().map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| format!("저장하지 못했습니다: {e}"))?;
    Ok(Some(path.display().to_string()))
}

/// 자동 저장된 요약 md 파일을 Finder·탐색기에서 보여 준다.
#[tauri::command]
fn reveal_summary(app: AppHandle, noko: State<'_, AppNoko>, recording_id: i64) -> Result<(), String> {
    let path = noko.0.summary_path(recording_id).map_err(|e| format!("{e:#}"))?;
    app.opener().reveal_item_in_dir(path).map_err(|e| e.to_string())
}

/// 로그를 앱 로그 폴더의 noko-note.log에 남긴다(창 앱이라 콘솔이 없을 수 있음).
fn init_logging(log_dir: &PathBuf) {
    let _ = std::fs::create_dir_all(log_dir);
    let file = std::fs::OpenOptions::new().create(true).append(true).open(log_dir.join("noko-note.log"));
    match file {
        Ok(file) => {
            let _ = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_target(false)
                .with_writer(std::sync::Mutex::new(file))
                .try_init();
        }
        Err(_) => {
            let _ = tracing_subscriber::fmt().with_target(false).try_init();
        }
    }
}

/// `system`이면 `None`(OS 설정을 따름).
fn to_window_theme(theme: &str) -> Option<Theme> {
    match theme {
        "light" => Some(Theme::Light),
        "dark" => Some(Theme::Dark),
        _ => None,
    }
}

/// 화면 테마를 바꾸면 창 제목 표시줄 색도 맞춘다.
#[tauri::command]
fn set_window_theme(window: WebviewWindow, theme: String) -> Result<(), String> {
    window.set_theme(to_window_theme(&theme)).map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![save_markdown, reveal_summary, set_window_theme])
        .setup(|app| {
            let app_dir = app.path().app_data_dir()?;
            init_logging(&app.path().app_log_dir().unwrap_or_else(|_| app_dir.join("logs")));
            noko_note::install_whisper_logging();

            let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
            let cfg = noko_note::Config {
                // 빈 포트를 골라 웹 버전(3000번)과 동시에 켜도 부딪히지 않게 한다.
                addr: "127.0.0.1:0".parse()?,
                data_dir: app_dir.join("data"),
                models_dir: app_dir.join("models"),
                whisper_model: env("WHISPER_MODEL").map(PathBuf::from),
                whisper_lang: env("WHISPER_LANG").unwrap_or_else(|| "ko".into()),
                claude_bin: env("CLAUDE_BIN").map(PathBuf::from),
                claude_model: env("CLAUDE_MODEL"),
            };
            tracing::info!("데이터 폴더: {}", app_dir.display());

            let noko = tauri::async_runtime::block_on(noko_note::Noko::start(cfg))?;
            let url = noko.url().parse()?;
            let theme = to_window_theme(&noko.theme());
            app.manage(AppNoko(noko));

            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("Noko-Note")
                .inner_size(1280.0, 820.0)
                .min_inner_size(900.0, 600.0)
                .theme(theme)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Noko-Note를 실행하지 못했습니다");
}
