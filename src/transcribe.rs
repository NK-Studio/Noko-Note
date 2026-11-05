//! 로컬 Whisper(whisper.cpp, Metal 가속)로 음성 파일을 텍스트로 바꾼다.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperVadParams,
};

use crate::audio;

/// 한 번 올려 둔 Whisper 모델. 받아쓰기마다 새 state를 만들어 쓴다.
pub struct Transcriber {
    ctx: WhisperContext,
    vad_model: Option<PathBuf>,
    language: String,
}

/// 받아쓰기 결과.
pub struct Transcript {
    /// `[mm:ss] 문장` 형식의 줄 목록.
    pub text: String,
    pub duration_sec: f64,
}

impl Transcriber {
    pub fn load(model: &Path, vad_model: Option<&Path>, language: &str) -> Result<Transcriber> {
        if !model.exists() {
            bail!("Whisper 모델 파일이 없습니다: {}", model.display());
        }
        let model_str = model.to_str().context("모델 경로가 UTF-8이 아닙니다")?;
        let mut params = WhisperContextParameters::default();
        params.use_gpu(cfg!(target_os = "macos")).flash_attn(cfg!(target_os = "macos"));
        let ctx = WhisperContext::new_with_params(model_str, params)
            .context("Whisper 모델을 불러오지 못했습니다")?;
        let vad_model = vad_model.filter(|p| p.exists()).map(Path::to_path_buf);
        Ok(Transcriber { ctx, vad_model, language: language.to_string() })
    }

    /// 오디오 파일을 받아쓴다. 블로킹 함수이므로 `spawn_blocking` 안에서 부른다.
    /// `names`는 녹음에 나오는 사람 이름으로, Whisper가 이름을 이 표기대로 받아쓰도록 힌트를 준다.
    /// `on_progress`는 0~100 사이 진행률로 불린다.
    pub fn transcribe(
        &self,
        audio_path: &Path,
        names: &[String],
        on_progress: impl FnMut(i32) + 'static,
    ) -> Result<Transcript> {
        let samples = audio::decode_to_16k_mono(audio_path)?;
        let duration_sec = samples.len() as f64 / audio::SAMPLE_RATE as f64;

        let mut params = FullParams::new(SamplingStrategy::BeamSearch { beam_size: 5, patience: -1.0 });
        params.set_language(Some(&self.language));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_n_threads(std::thread::available_parallelism().map_or(4, |n| n.get() as i32));
        params.set_progress_callback_safe(on_progress);
        // 처음 문맥으로 이름을 넣어 두면 Whisper가 같은 표기를 따라 쓰는 경향이 있다.
        let initial_prompt = if names.is_empty() { String::new() } else { format!("참석자: {}.", names.join(", ")) };
        if !initial_prompt.is_empty() {
            params.set_initial_prompt(&initial_prompt);
        }

        // 말소리가 없는 구간을 건너뛰어, 조용한 구간에서 없는 말을 지어내는 현상을 줄인다.
        let vad_path;
        if let Some(vad) = &self.vad_model {
            vad_path = vad.to_string_lossy().into_owned();
            params.set_vad_model_path(Some(&vad_path));
            params.set_vad_params(WhisperVadParams::new());
            params.enable_vad(true);
        }

        let mut state = self.ctx.create_state().context("Whisper state를 만들지 못했습니다")?;
        state.full(params, &samples).context("받아쓰기에 실패했습니다")?;

        let mut lines = Vec::new();
        for segment in state.as_iter() {
            let text = segment.to_str_lossy()?.trim().to_string();
            if text.is_empty() {
                continue;
            }
            // 타임스탬프 단위는 1/100초.
            let start = segment.start_timestamp() / 100;
            lines.push(format!("[{:02}:{:02}] {}", start / 60, start % 60, text));
        }
        Ok(Transcript { text: lines.join("\n"), duration_sec })
    }
}
