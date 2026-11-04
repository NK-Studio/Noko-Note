//! Noko-Note: 음성 파일을 로컬 Whisper로 받아쓰고, 채널(방)마다 정한 컨셉으로
//! Claude 계정을 통해 요약해 화면·md 파일·Slack·Discord로 전달하는 로컬 웹 앱.

mod db;
mod notify;
mod pipeline;
mod summarize;
mod transcribe;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;

use db::{ChannelInput, Db};
use pipeline::Pipeline;
use summarize::Summarizer;
use transcribe::Transcriber;

const INDEX_HTML: &str = include_str!("../static/index.html");
/// 업로드 최대 크기(2GB).
const MAX_UPLOAD: usize = 2 * 1024 * 1024 * 1024;
const AUDIO_EXTS: &[&str] = &["m4a", "mp3", "wav", "aac", "flac", "ogg", "opus", "webm", "mp4", "mov", "caf", "amr"];

#[derive(Clone)]
struct AppState {
    db: Db,
    pipeline: Pipeline,
    summarizer: Summarizer,
    http: reqwest::Client,
    uploads_dir: PathBuf,
    whisper_ready: bool,
}

/// 환경 변수(.env 포함)에서 읽는 설정.
struct Config {
    addr: SocketAddr,
    data_dir: PathBuf,
    whisper_model: PathBuf,
    whisper_vad_model: PathBuf,
    whisper_lang: String,
    claude_bin: String,
    claude_model: Option<String>,
}

impl Config {
    fn from_env() -> Result<Config> {
        let var = |k: &str, d: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| d.to_string());
        Ok(Config {
            addr: var("NOKO_ADDR", "127.0.0.1:3000").parse().context("NOKO_ADDR 형식이 잘못되었습니다")?,
            data_dir: var("NOKO_DATA_DIR", "data").into(),
            whisper_model: var("WHISPER_MODEL", "models/ggml-large-v3-turbo.bin").into(),
            whisper_vad_model: var("WHISPER_VAD_MODEL", "models/ggml-silero-v5.1.2.bin").into(),
            whisper_lang: var("WHISPER_LANG", "ko"),
            claude_bin: var("CLAUDE_BIN", "claude"),
            claude_model: std::env::var("CLAUDE_MODEL").ok().filter(|v| !v.trim().is_empty()),
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt().with_target(false).init();
    // whisper.cpp 내부 로그가 터미널을 덮지 않도록 tracing 쪽으로 돌린다(기본은 출력 안 함).
    whisper_rs::install_logging_hooks();

    let cfg = Config::from_env()?;
    let uploads_dir = cfg.data_dir.join("uploads");
    let summaries_dir = cfg.data_dir.join("summaries");
    std::fs::create_dir_all(&uploads_dir)?;
    std::fs::create_dir_all(&summaries_dir)?;

    let db = Db::open(&cfg.data_dir.join("noko.db"))?;
    if db.list_channels()?.is_empty() {
        db.create_channel(&ChannelInput {
            name: "일반".into(),
            concept: String::new(),
            slack_webhook: String::new(),
            discord_webhook: String::new(),
        })?;
    }

    tracing::info!("Whisper 모델 불러오는 중: {}", cfg.whisper_model.display());
    let (model, vad, lang) = (cfg.whisper_model.clone(), cfg.whisper_vad_model.clone(), cfg.whisper_lang.clone());
    let transcriber = match tokio::task::spawn_blocking(move || Transcriber::load(&model, Some(&vad), &lang)).await? {
        Ok(t) => Some(Arc::new(t)),
        Err(e) => {
            tracing::error!("{e:#}");
            None
        }
    };

    let summarizer = Summarizer::new(
        cfg.claude_bin.clone(),
        cfg.claude_model.clone(),
        cfg.data_dir.join("claude-work"),
    );
    let whisper_ready = transcriber.is_some();
    let pipeline = Pipeline::start(db.clone(), summarizer.clone(), transcriber, summaries_dir);
    pipeline.resume_unfinished()?;

    let state = AppState {
        db,
        pipeline,
        summarizer,
        http: reqwest::Client::new(),
        uploads_dir,
        whisper_ready,
    };

    let app = Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/api/status", get(status))
        .route("/api/claude/login", post(claude_login))
        .route("/api/channels", get(list_channels).post(create_channel))
        .route("/api/channels/{id}", get(get_channel).put(update_channel).delete(delete_channel))
        .route("/api/channels/{id}/recordings", get(list_recordings).post(upload_recording))
        .route("/api/recordings/{id}", get(get_recording).delete(delete_recording))
        .route("/api/recordings/{id}/summarize", post(resummarize))
        .route("/api/recordings/{id}/summary.md", get(download_markdown))
        .route("/api/recordings/{id}/send", post(send_summary))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(cfg.addr).await?;
    tracing::info!("Noko-Note 실행 중: http://{}", cfg.addr);
    axum::serve(listener, app).await?;
    Ok(())
}

// ---- 오류 응답 ----

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> ApiError {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

type ApiResult<T> = Result<T, ApiError>;

fn not_found(what: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, format!("{what}을(를) 찾을 수 없습니다"))
}

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

// ---- 상태·로그인 ----

async fn status(State(s): State<AppState>) -> Json<serde_json::Value> {
    let claude = s.summarizer.auth_status().await;
    Json(json!({ "whisper_ready": s.whisper_ready, "claude": claude }))
}

/// 브라우저에서 claude.ai 로그인 페이지를 열어 Claude 계정을 연결한다.
async fn claude_login(State(s): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    s.summarizer.start_login()?;
    Ok(Json(json!({ "ok": true })))
}

// ---- 채널 ----

fn validate_channel(input: &ChannelInput) -> ApiResult<()> {
    if input.name.trim().is_empty() {
        return Err(bad_request("채널 이름을 입력하세요"));
    }
    for (label, url) in [("Slack", &input.slack_webhook), ("Discord", &input.discord_webhook)] {
        let url = url.trim();
        if !url.is_empty() && !url.starts_with("https://") {
            return Err(bad_request(format!("{label} 웹훅 주소는 https://로 시작해야 합니다")));
        }
    }
    Ok(())
}

async fn list_channels(State(s): State<AppState>) -> ApiResult<Json<Vec<db::Channel>>> {
    Ok(Json(s.db.list_channels()?))
}

async fn get_channel(State(s): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<db::Channel>> {
    s.db.get_channel(id)?.map(Json).ok_or_else(|| not_found("채널"))
}

async fn create_channel(State(s): State<AppState>, Json(input): Json<ChannelInput>) -> ApiResult<Json<db::Channel>> {
    validate_channel(&input)?;
    Ok(Json(s.db.create_channel(&input)?))
}

async fn update_channel(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<ChannelInput>,
) -> ApiResult<Json<db::Channel>> {
    validate_channel(&input)?;
    s.db.update_channel(id, &input)?.map(Json).ok_or_else(|| not_found("채널"))
}

async fn delete_channel(State(s): State<AppState>, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    // 채널의 녹음 파일도 함께 지운다(요약 md 파일은 남겨 둔다).
    for rec in s.db.list_recordings(id)? {
        let _ = tokio::fs::remove_file(&rec.audio_path).await;
    }
    if s.db.delete_channel(id)? { Ok(StatusCode::NO_CONTENT) } else { Err(not_found("채널")) }
}

// ---- 녹음 ----

async fn list_recordings(State(s): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Vec<db::Recording>>> {
    Ok(Json(s.db.list_recordings(id)?))
}

async fn get_recording(State(s): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<db::Recording>> {
    s.db.get_recording(id)?.map(Json).ok_or_else(|| not_found("녹음"))
}

/// multipart 필드: `file`(필수), `title`, `instruction`.
async fn upload_recording(
    State(s): State<AppState>,
    Path(channel_id): Path<i64>,
    mut multipart: Multipart,
) -> ApiResult<Json<db::Recording>> {
    s.db.get_channel(channel_id)?.ok_or_else(|| not_found("채널"))?;

    let mut title = String::new();
    let mut instruction = String::new();
    let mut saved: Option<(PathBuf, String)> = None;

    while let Some(mut field) = multipart.next_field().await.map_err(|e| bad_request(e.to_string()))? {
        match field.name().unwrap_or_default() {
            "title" => title = field.text().await.map_err(|e| bad_request(e.to_string()))?,
            "instruction" => instruction = field.text().await.map_err(|e| bad_request(e.to_string()))?,
            "file" => {
                let original = field.file_name().unwrap_or("audio").to_string();
                let ext = std::path::Path::new(&original)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_lowercase)
                    .unwrap_or_default();
                if !AUDIO_EXTS.contains(&ext.as_str()) {
                    return Err(bad_request(format!("지원하지 않는 파일 형식입니다: .{ext}")));
                }
                let path = s.uploads_dir.join(format!("{}.{ext}", uuid::Uuid::new_v4()));
                let mut file = tokio::fs::File::create(&path).await.map_err(anyhow::Error::from)?;
                while let Some(chunk) = field.chunk().await.map_err(|e| bad_request(e.to_string()))? {
                    file.write_all(&chunk).await.map_err(anyhow::Error::from)?;
                }
                file.flush().await.map_err(anyhow::Error::from)?;
                saved = Some((path, original));
            }
            _ => {}
        }
    }

    let (path, original) = saved.ok_or_else(|| bad_request("음성 파일(file)을 첨부하세요"))?;
    if title.trim().is_empty() {
        title = std::path::Path::new(&original)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "녹음".into());
    }
    let id = s.db.create_recording(channel_id, title.trim(), &path.to_string_lossy(), instruction.trim())?;
    s.pipeline.enqueue(id);
    s.db.get_recording(id)?.map(Json).ok_or_else(|| not_found("녹음"))
}

async fn delete_recording(State(s): State<AppState>, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let rec = s.db.delete_recording(id)?.ok_or_else(|| not_found("녹음"))?;
    let _ = tokio::fs::remove_file(&rec.audio_path).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ResummarizeInput {
    #[serde(default)]
    instruction: String,
}

/// 같은 녹취록을 새 지시문으로 다시 요약한다.
async fn resummarize(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<ResummarizeInput>,
) -> ApiResult<Json<db::Recording>> {
    let rec = s.db.get_recording(id)?.ok_or_else(|| not_found("녹음"))?;
    if rec.transcript.trim().is_empty() {
        return Err(bad_request("아직 받아쓰기가 끝나지 않았습니다"));
    }
    if matches!(rec.status, db::Status::Summarizing | db::Status::Transcribing | db::Status::Queued) {
        return Err(bad_request("이미 처리 중입니다"));
    }
    s.db.set_instruction(id, input.instruction.trim())?;
    s.db.set_status(id, db::Status::Summarizing)?;
    s.pipeline.resummarize(id);
    s.db.get_recording(id)?.map(Json).ok_or_else(|| not_found("녹음"))
}

async fn download_markdown(State(s): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response> {
    let rec = s.db.get_recording(id)?.ok_or_else(|| not_found("녹음"))?;
    if rec.summary.trim().is_empty() {
        return Err(bad_request("아직 요약이 없습니다"));
    }
    let channel = s.db.get_channel(rec.channel_id)?.ok_or_else(|| not_found("채널"))?;
    let body = pipeline::markdown_document(&channel, &rec, &rec.summary);
    let file_name = format!("{}.md", pipeline::safe_file_name(&rec.title));

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/markdown; charset=utf-8"));
    // 한글 파일 이름은 RFC 5987 방식(filename*)으로 보낸다.
    let disposition = format!("attachment; filename=\"summary.md\"; filename*=UTF-8''{}", percent_encode(&file_name));
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).map_err(anyhow::Error::from)?,
    );
    Ok((headers, body).into_response())
}

#[derive(Deserialize)]
struct SendInput {
    target: notify::Target,
}

async fn send_summary(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SendInput>,
) -> ApiResult<Json<serde_json::Value>> {
    let rec = s.db.get_recording(id)?.ok_or_else(|| not_found("녹음"))?;
    if rec.summary.trim().is_empty() {
        return Err(bad_request("아직 요약이 없습니다"));
    }
    let channel = s.db.get_channel(rec.channel_id)?.ok_or_else(|| not_found("채널"))?;
    let webhook = match input.target {
        notify::Target::Slack => &channel.slack_webhook,
        notify::Target::Discord => &channel.discord_webhook,
    };
    let message = pipeline::markdown_document(&channel, &rec, &rec.summary);
    notify::send(&s.http, input.target, webhook, &message)
        .await
        .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
