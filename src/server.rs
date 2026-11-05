//! 로컬 웹 서버(axum): 화면과 JSON API를 제공한다.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;

use crate::db::{self, ChannelInput, Db};
use crate::models::{WhisperSlot, WhisperStatus};
use crate::notify;
use crate::pipeline::{self, Pipeline};
use crate::summarize::{self, Summarizer};
use crate::Config;

const INDEX_HTML: &str = include_str!("../static/index.html");
const MARKED_JS: &str = include_str!("../static/vendor/marked.min.js");
const PURIFY_JS: &str = include_str!("../static/vendor/purify.min.js");
/// 업로드 최대 크기(2GB).
const MAX_UPLOAD: usize = 2 * 1024 * 1024 * 1024;
const AUDIO_EXTS: &[&str] = &["m4a", "mp3", "wav", "aac", "flac", "ogg", "opus", "webm", "mp4", "mov", "caf", "amr", "aif", "aiff"];

#[derive(Clone)]
struct AppState {
    db: Db,
    pipeline: Pipeline,
    summarizer: Summarizer,
    whisper: Arc<WhisperSlot>,
    http: reqwest::Client,
    uploads_dir: PathBuf,
}

/// 실행 중인 Noko-Note 서버. 데스크톱 앱은 이 핸들로 md 저장 등을 직접 처리한다.
#[derive(Clone)]
pub struct Noko {
    pub addr: SocketAddr,
    state: AppState,
}

impl Noko {
    /// 서버를 띄우고 바로 돌려준다. Whisper 모델은 백그라운드에서 불러온다.
    pub async fn start(cfg: Config) -> Result<Noko> {
        let uploads_dir = cfg.data_dir.join("uploads");
        let summaries_dir = cfg.data_dir.join("summaries");
        std::fs::create_dir_all(&uploads_dir)
            .with_context(|| format!("데이터 폴더를 만들 수 없습니다: {}", cfg.data_dir.display()))?;
        std::fs::create_dir_all(&summaries_dir)?;

        let db = Db::open(&cfg.data_dir.join("noko.db"))?;
        if db.list_channels()?.is_empty() {
            db.create_channel(&ChannelInput {
                name: "일반".into(),
                concept: String::new(),
                participants: Vec::new(),
                slack_webhook: String::new(),
                discord_webhook: String::new(),
            })?;
        }

        let whisper = WhisperSlot::new(cfg.models_dir.clone(), cfg.whisper_lang.clone(), cfg.whisper_model.clone());
        tokio::spawn(whisper.clone().load());

        let summarizer = Summarizer::new(cfg.claude_bin.clone(), cfg.claude_model.clone(), cfg.data_dir.join("claude-work"));
        let pipeline = Pipeline::start(db.clone(), summarizer.clone(), whisper.clone(), summaries_dir);
        pipeline.resume_unfinished()?;

        let state = AppState {
            db,
            pipeline,
            summarizer,
            whisper,
            http: reqwest::Client::builder().user_agent("noko-note").build()?,
            uploads_dir,
        };

        let listener = tokio::net::TcpListener::bind(cfg.addr)
            .await
            .with_context(|| format!("{} 주소를 쓸 수 없습니다(이미 실행 중인지 확인하세요)", cfg.addr))?;
        let addr = listener.local_addr()?;
        let loopback = addr.ip().is_loopback();

        let app = Router::new()
            .route("/", get(index))
            .route("/api/settings/theme", put(set_theme))
            .route("/api/settings/summary-model", put(set_summary_model))
            .route("/vendor/marked.min.js", get(|| async { javascript(MARKED_JS) }))
            .route("/vendor/purify.min.js", get(|| async { javascript(PURIFY_JS) }))
            .route("/api/status", get(status))
            .route("/api/claude/login", post(claude_login))
            .route("/api/whisper/download", post(download_model))
            .route("/api/channels", get(list_channels).post(create_channel))
            .route("/api/channels/{id}", get(get_channel).put(update_channel).delete(delete_channel))
            .route("/api/channels/{id}/recordings", get(list_recordings).post(upload_recording))
            .route("/api/recordings/{id}", get(get_recording).delete(delete_recording))
            .route("/api/recordings/{id}/summarize", post(resummarize))
            .route("/api/recordings/{id}/summary.md", get(download_markdown))
            .route("/api/recordings/{id}/send", post(send_summary))
            .layer(DefaultBodyLimit::max(MAX_UPLOAD))
            .layer(middleware::from_fn(move |req, next| guard(loopback, req, next)))
            .with_state(state.clone());

        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("서버가 멈췄습니다: {e:#}");
            }
        });
        tracing::info!("Noko-Note 실행 중: http://{addr}");
        Ok(Noko { addr, state })
    }

    /// 저장된 화면 테마: `system`, `light`, `dark`.
    pub fn theme(&self) -> String {
        load_theme(&self.state.db)
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// md 문서 (추천 파일 이름, 내용).
    pub fn markdown(&self, recording_id: i64) -> Result<(String, String)> {
        let rec = self.state.db.get_recording(recording_id)?.context("녹음을 찾을 수 없습니다")?;
        anyhow::ensure!(!rec.summary.trim().is_empty(), "아직 요약이 없습니다");
        let channel = self.state.db.get_channel(rec.channel_id)?.context("채널을 찾을 수 없습니다")?;
        Ok((
            format!("{}.md", pipeline::safe_file_name(&rec.title)),
            pipeline::markdown_document(&channel, &rec, &rec.summary),
        ))
    }

    /// 자동 저장된 요약 md 파일 위치.
    pub fn summary_path(&self, recording_id: i64) -> Result<PathBuf> {
        let rec = self.state.db.get_recording(recording_id)?.context("녹음을 찾을 수 없습니다")?;
        anyhow::ensure!(!rec.summary_path.is_empty(), "아직 저장된 요약 파일이 없습니다");
        let path = PathBuf::from(rec.summary_path);
        Ok(std::fs::canonicalize(&path).unwrap_or(path))
    }
}

const THEMES: [&str; 3] = ["system", "light", "dark"];

fn load_theme(db: &Db) -> String {
    db.get_setting("theme")
        .ok()
        .flatten()
        .filter(|t| THEMES.contains(&t.as_str()))
        .unwrap_or_else(|| "system".into())
}

/// 저장된 테마를 `<html data-theme>`에 미리 넣어, 페이지가 뜰 때 색이 깜빡이지 않게 한다.
async fn index(State(s): State<AppState>) -> Html<String> {
    let theme = load_theme(&s.db);
    Html(INDEX_HTML.replacen("<html lang=\"ko\">", &format!("<html lang=\"ko\" data-theme=\"{theme}\">"), 1))
}

#[derive(Deserialize)]
struct ThemeInput {
    theme: String,
}

async fn set_theme(State(s): State<AppState>, Json(input): Json<ThemeInput>) -> ApiResult<StatusCode> {
    if !THEMES.contains(&input.theme.as_str()) {
        return Err(bad_request("테마는 system, light, dark 중 하나여야 합니다"));
    }
    s.db.set_setting("theme", &input.theme)?;
    Ok(StatusCode::NO_CONTENT)
}

const SUMMARY_MODEL_KEY: &str = "summary_model";

/// 화면에서 고른 요약 모델. 고른 적이 없거나 목록에서 사라진 모델이면 `None`.
pub(crate) fn load_summary_model(db: &Db) -> Option<String> {
    db.get_setting(SUMMARY_MODEL_KEY)
        .ok()
        .flatten()
        .filter(|m| summarize::is_known_model(m))
}

#[derive(Deserialize)]
struct SummaryModelInput {
    model: String,
}

async fn set_summary_model(State(s): State<AppState>, Json(input): Json<SummaryModelInput>) -> ApiResult<StatusCode> {
    if !summarize::is_known_model(&input.model) {
        return Err(bad_request("목록에 없는 모델입니다"));
    }
    s.db.set_setting(SUMMARY_MODEL_KEY, &input.model)?;
    Ok(StatusCode::NO_CONTENT)
}

fn javascript(body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], body).into_response()
}

/// 다른 웹사이트가 이 로컬 서버로 요청을 보내지 못하게 막는다.
/// - 루프백으로 열었으면 Host가 127.0.0.1/localhost일 때만 받는다(DNS 재바인딩 방지).
/// - 읽기 외 요청은 Origin이 있으면 이 서버 자신과 같아야 한다(CSRF 방지).
async fn guard(loopback: bool, req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if loopback {
        let name = host.rsplit_once(':').map_or(host.as_str(), |(h, _)| h);
        if !matches!(name, "127.0.0.1" | "localhost" | "[::1]") {
            return (StatusCode::FORBIDDEN, "허용되지 않은 Host입니다").into_response();
        }
    }
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe {
        if let Some(origin) = req.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
            if origin != format!("http://{host}") {
                return (StatusCode::FORBIDDEN, "다른 사이트에서 온 요청은 받지 않습니다").into_response();
            }
        }
    }
    next.run(req).await
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
    let whisper = s.whisper.status();
    Json(json!({
        "whisper_ready": matches!(whisper, WhisperStatus::Ready { .. }),
        "whisper": whisper,
        "models": s.whisper.choices(),
        "claude": claude,
        "summary_model": s.summarizer.resolve_model(load_summary_model(&s.db).as_deref()),
        "summary_models": summarize::MODEL_CHOICES,
        "default_summary_model": summarize::DEFAULT_MODEL,
    }))
}

#[derive(Deserialize)]
struct DownloadInput {
    model: String,
}

/// 앱 안에서 Whisper 모델을 내려받는다. 진행 상황은 `/api/status`로 확인한다.
async fn download_model(State(s): State<AppState>, Json(input): Json<DownloadInput>) -> ApiResult<StatusCode> {
    if s.whisper.is_preparing() {
        return Err(bad_request("이미 모델을 준비하는 중입니다"));
    }
    tokio::spawn(s.whisper.clone().download(s.http.clone(), input.model));
    Ok(StatusCode::ACCEPTED)
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
    if input.participants.len() > 100 {
        return Err(bad_request("참여자는 100명까지 넣을 수 있습니다"));
    }
    if input.participants.iter().any(|n| n.chars().count() > 50) {
        return Err(bad_request("참여자 이름은 50자 이하로 입력하세요"));
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

/// 같은 녹취록을 새 지시문으로 다시 요약한다. 받아쓰기가 없으면 받아쓰기부터 다시 한다.
async fn resummarize(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<ResummarizeInput>,
) -> ApiResult<Json<db::Recording>> {
    let rec = s.db.get_recording(id)?.ok_or_else(|| not_found("녹음"))?;
    if matches!(rec.status, db::Status::Summarizing | db::Status::Transcribing | db::Status::Queued) {
        return Err(bad_request("이미 처리 중입니다"));
    }
    s.db.set_instruction(id, input.instruction.trim())?;
    if rec.transcript.trim().is_empty() {
        // 받아쓰기가 실패했던 녹음은 받아쓰기부터 다시 한다.
        s.db.set_status(id, db::Status::Queued)?;
        s.pipeline.enqueue(id);
    } else {
        s.db.set_status(id, db::Status::Summarizing)?;
        s.pipeline.resummarize(id);
    }
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
