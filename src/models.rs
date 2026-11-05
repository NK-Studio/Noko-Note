//! Whisper 모델 관리: 설치 여부 확인, 앱 안에서 내려받기, 불러오기.
//! 서버는 모델 없이도 먼저 뜨고, 모델은 백그라운드에서 불러오거나 내려받는다.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::transcribe::Transcriber;

pub struct ModelInfo {
    pub key: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub size_mb: u32,
    pub label: &'static str,
}

/// 선호 순서. 여러 개가 설치되어 있으면 앞의 것을 쓴다.
pub const MODELS: &[ModelInfo] = &[
    ModelInfo {
        key: "large-v3-turbo",
        file: "ggml-large-v3-turbo.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        size_mb: 1550,
        label: "정확도 우선 (Apple Silicon Mac 권장)",
    },
    ModelInfo {
        key: "large-v3-turbo-q5_0",
        file: "ggml-large-v3-turbo-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
        size_mb: 547,
        label: "가벼움 (Windows·저사양 PC 권장)",
    },
];

const VAD_FILE: &str = "ggml-silero-v5.1.2.bin";
const VAD_URL: &str = "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WhisperStatus {
    Loading { model: String },
    Ready { model: String },
    Missing,
    Downloading { model: String, downloaded: u64, total: u64 },
    Failed { error: String },
}

#[derive(Serialize)]
pub struct ModelChoice {
    key: &'static str,
    label: &'static str,
    size_mb: u32,
    installed: bool,
}

pub struct WhisperSlot {
    models_dir: PathBuf,
    language: String,
    /// `WHISPER_MODEL`로 직접 지정한 모델 파일.
    override_model: Option<PathBuf>,
    inner: RwLock<(WhisperStatus, Option<Arc<Transcriber>>)>,
}

impl WhisperSlot {
    pub fn new(models_dir: PathBuf, language: String, override_model: Option<PathBuf>) -> Arc<WhisperSlot> {
        Arc::new(WhisperSlot {
            models_dir,
            language,
            override_model,
            inner: RwLock::new((WhisperStatus::Missing, None)),
        })
    }

    pub fn status(&self) -> WhisperStatus {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).0.clone()
    }

    pub fn transcriber(&self) -> Option<Arc<Transcriber>> {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).1.clone()
    }

    /// 불러오는 중이거나 내려받는 중이면 true. 받아쓰기 대기열은 이때 기다린다.
    pub fn is_preparing(&self) -> bool {
        matches!(self.status(), WhisperStatus::Loading { .. } | WhisperStatus::Downloading { .. })
    }

    fn set(&self, status: WhisperStatus, transcriber: Option<Arc<Transcriber>>) {
        *self.inner.write().unwrap_or_else(|e| e.into_inner()) = (status, transcriber);
    }

    pub fn choices(&self) -> Vec<ModelChoice> {
        MODELS
            .iter()
            .map(|m| ModelChoice {
                key: m.key,
                label: m.label,
                size_mb: m.size_mb,
                installed: self.models_dir.join(m.file).is_file(),
            })
            .collect()
    }

    fn installed_model(&self) -> Option<PathBuf> {
        if let Some(p) = &self.override_model {
            return Some(p.clone());
        }
        MODELS.iter().map(|m| self.models_dir.join(m.file)).find(|p| p.is_file())
    }

    /// 설치된 모델을 불러온다. 없으면 `Missing` 상태가 된다.
    pub async fn load(self: Arc<Self>) {
        let Some(model) = self.installed_model() else {
            self.set(WhisperStatus::Missing, None);
            return;
        };
        let name = model.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.set(WhisperStatus::Loading { model: name.clone() }, None);
        tracing::info!("Whisper 모델 불러오는 중: {}", model.display());

        let vad = self.models_dir.join(VAD_FILE);
        let lang = self.language.clone();
        let result = tokio::task::spawn_blocking(move || Transcriber::load(&model, Some(&vad), &lang)).await;
        match result {
            Ok(Ok(t)) => {
                tracing::info!("Whisper 준비됨: {name}");
                self.set(WhisperStatus::Ready { model: name }, Some(Arc::new(t)));
            }
            Ok(Err(e)) => self.set(WhisperStatus::Failed { error: format!("{e:#}") }, None),
            Err(e) => self.set(WhisperStatus::Failed { error: e.to_string() }, None),
        }
    }

    /// 모델(과 VAD 모델)을 내려받은 뒤 불러온다.
    pub async fn download(self: Arc<Self>, client: reqwest::Client, key: String) {
        let Some(info) = MODELS.iter().find(|m| m.key == key) else {
            self.set(WhisperStatus::Failed { error: format!("알 수 없는 모델입니다: {key}") }, None);
            return;
        };
        let result = async {
            tokio::fs::create_dir_all(&self.models_dir).await?;
            let vad = self.models_dir.join(VAD_FILE);
            if !vad.is_file() {
                self.fetch(&client, VAD_URL, &vad, "음성 구간 감지(VAD) 모델").await?;
            }
            let target = self.models_dir.join(info.file);
            if !target.is_file() {
                self.fetch(&client, info.url, &target, info.file).await?;
            }
            anyhow::Ok(())
        }
        .await;

        match result {
            Ok(()) => self.load().await,
            Err(e) => {
                tracing::error!("모델 내려받기 실패: {e:#}");
                self.set(WhisperStatus::Failed { error: format!("모델 내려받기 실패: {e:#}") }, None);
            }
        }
    }

    async fn fetch(&self, client: &reqwest::Client, url: &str, dest: &Path, label: &str) -> Result<()> {
        let resp = client.get(url).send().await.context("내려받기 서버에 연결하지 못했습니다")?;
        if !resp.status().is_success() {
            bail!("내려받기 실패 ({})", resp.status());
        }
        let total = resp.content_length().unwrap_or(0);
        let part = dest.with_extension("part");
        let mut file = tokio::fs::File::create(&part).await?;
        let mut downloaded = 0u64;
        let mut stream = resp.bytes_stream();
        self.set(WhisperStatus::Downloading { model: label.into(), downloaded, total }, None);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("내려받는 중 연결이 끊겼습니다")?;
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;
            self.set(WhisperStatus::Downloading { model: label.into(), downloaded, total }, None);
        }
        file.flush().await?;
        drop(file);
        if total > 0 && downloaded != total {
            let _ = tokio::fs::remove_file(&part).await;
            bail!("파일이 덜 받아졌습니다 ({downloaded}/{total} 바이트)");
        }
        tokio::fs::rename(&part, dest).await?;
        Ok(())
    }
}
