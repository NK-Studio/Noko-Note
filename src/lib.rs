//! Noko-Note 핵심 라이브러리.
//!
//! 음성 파일을 로컬 Whisper로 받아쓰고, 채널(방)마다 정한 컨셉으로 Claude 계정을 통해
//! 요약해 화면·md 파일·Slack·Discord로 전달한다. 웹 버전(`noko-note` 바이너리)과
//! Tauri 데스크톱 앱(`src-tauri`)이 같은 서버를 쓴다.

mod audio;
mod db;
mod models;
mod notify;
mod pipeline;
mod server;
mod summarize;
mod tools;
mod transcribe;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};

pub use server::Noko;

/// 서버 설정.
#[derive(Debug, Clone)]
pub struct Config {
    /// 들을 주소. 포트를 0으로 주면 비어 있는 포트를 고른다(데스크톱 앱).
    pub addr: SocketAddr,
    /// DB·업로드·요약 md 파일이 저장되는 폴더.
    pub data_dir: PathBuf,
    /// Whisper 모델 폴더.
    pub models_dir: PathBuf,
    /// 특정 모델 파일을 직접 쓸 때.
    pub whisper_model: Option<PathBuf>,
    pub whisper_lang: String,
    /// claude 실행 파일. 없으면 자동으로 찾는다.
    pub claude_bin: Option<PathBuf>,
    pub claude_model: Option<String>,
}

impl Config {
    /// 환경 변수(.env 포함)에서 읽는다. 웹 버전 기본값은 현재 폴더의 `data/`, `models/`.
    pub fn from_env() -> Result<Config> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Ok(Config {
            addr: var("NOKO_ADDR")
                .unwrap_or_else(|| "127.0.0.1:3000".into())
                .parse()
                .context("NOKO_ADDR 형식이 잘못되었습니다")?,
            data_dir: var("NOKO_DATA_DIR").unwrap_or_else(|| "data".into()).into(),
            models_dir: var("NOKO_MODELS_DIR").unwrap_or_else(|| "models".into()).into(),
            whisper_model: var("WHISPER_MODEL").map(PathBuf::from),
            whisper_lang: var("WHISPER_LANG").unwrap_or_else(|| "ko".into()),
            claude_bin: var("CLAUDE_BIN").map(PathBuf::from),
            claude_model: var("CLAUDE_MODEL"),
        })
    }
}

/// whisper.cpp 내부 로그를 tracing으로 돌린다(기본 설정에서는 출력하지 않음).
pub fn install_whisper_logging() {
    whisper_rs::install_logging_hooks();
}
