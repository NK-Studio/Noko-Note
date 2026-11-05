//! 처리 흐름: 음성 파일 → Whisper 받아쓰기 → 채널 컨셉으로 Claude 요약 → md 파일 저장.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::mpsc;

use crate::db::{Channel, Db, Recording, Status};
use crate::summarize::{SummaryRequest, Summarizer};
use crate::models::WhisperSlot;
use crate::transcribe::Transcriber;

#[derive(Clone)]
pub struct Pipeline {
    db: Db,
    summarizer: Summarizer,
    summaries_dir: PathBuf,
    /// 받아쓰기 대기열. GPU를 한 번에 하나씩 쓰도록 일꾼 하나가 순서대로 처리한다.
    transcribe_tx: mpsc::UnboundedSender<i64>,
}

impl Pipeline {
    pub fn start(
        db: Db,
        summarizer: Summarizer,
        whisper: Arc<WhisperSlot>,
        summaries_dir: PathBuf,
    ) -> Pipeline {
        let (tx, rx) = mpsc::unbounded_channel();
        let pipeline = Pipeline { db, summarizer, summaries_dir, transcribe_tx: tx };
        tokio::spawn(pipeline.clone().transcribe_worker(rx, whisper));
        pipeline
    }

    /// 새로 올린 녹음을 대기열에 넣는다.
    pub fn enqueue(&self, recording_id: i64) {
        let _ = self.transcribe_tx.send(recording_id);
    }

    /// 서버 재시작 전에 끝나지 않은 작업을 다시 잡는다.
    pub fn resume_unfinished(&self) -> Result<()> {
        for id in self.db.unfinished_recordings()? {
            let rec = self.db.get_recording(id)?.context("녹음을 찾을 수 없습니다")?;
            if rec.transcript.is_empty() {
                self.enqueue(id);
            } else {
                self.resummarize(id);
            }
        }
        Ok(())
    }

    /// 이미 받아쓴 녹음을 (새 지시문으로) 다시 요약한다. 받아쓰기는 다시 하지 않는다.
    pub fn resummarize(&self, recording_id: i64) {
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this.summarize(recording_id).await {
                tracing::error!(recording_id, "요약 실패: {e:#}");
                let _ = this.db.set_error(recording_id, &format!("{e:#}"));
            }
        });
    }

    async fn transcribe_worker(
        self,
        mut rx: mpsc::UnboundedReceiver<i64>,
        whisper: Arc<WhisperSlot>,
    ) {
        while let Some(id) = rx.recv().await {
            // 모델을 불러오거나 내려받는 중이면 끝날 때까지 기다린다.
            while whisper.is_preparing() {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            let result = match whisper.transcriber() {
                Some(t) => self.transcribe(id, t).await,
                None => Err(anyhow::anyhow!(
                    "Whisper 모델이 준비되지 않았습니다. 화면 위쪽에서 모델을 내려받은 뒤 다시 올려 주세요"
                )),
            };
            match result {
                Ok(()) => self.resummarize(id),
                Err(e) => {
                    tracing::error!(recording_id = id, "받아쓰기 실패: {e:#}");
                    let _ = self.db.set_error(id, &format!("{e:#}"));
                }
            }
        }
    }

    async fn transcribe(&self, id: i64, transcriber: Arc<Transcriber>) -> Result<()> {
        let rec = self.db.get_recording(id)?.context("녹음을 찾을 수 없습니다")?;
        let participants = self
            .db
            .get_channel(rec.channel_id)?
            .map(|c| c.participants)
            .unwrap_or_default();
        self.db.set_status(id, Status::Transcribing)?;
        self.db.set_progress(id, 0)?;
        tracing::info!(recording_id = id, "받아쓰기 시작: {}", rec.title);

        let db = self.db.clone();
        let audio = PathBuf::from(&rec.audio_path);
        let transcript = tokio::task::spawn_blocking(move || {
            let mut last = -1;
            transcriber.transcribe(&audio, &participants, move |p| {
                // 진행률이 바뀔 때만 DB에 쓴다.
                if p != last {
                    last = p;
                    let _ = db.set_progress(id, p.clamp(0, 99) as i64);
                }
            })
        })
        .await??;

        self.db.set_transcript(id, &transcript.text, transcript.duration_sec)?;
        tracing::info!(recording_id = id, "받아쓰기 완료 ({:.0}초 분량)", transcript.duration_sec);
        Ok(())
    }

    async fn summarize(&self, id: i64) -> Result<()> {
        let rec = self.db.get_recording(id)?.context("녹음을 찾을 수 없습니다")?;
        let channel = self.db.get_channel(rec.channel_id)?.context("채널을 찾을 수 없습니다")?;
        if rec.transcript.trim().is_empty() {
            anyhow::bail!("녹취록이 비어 있어 요약할 수 없습니다");
        }
        self.db.set_status(id, Status::Summarizing)?;
        tracing::info!(recording_id = id, "요약 시작 (채널: {})", channel.name);

        let summary = self
            .summarizer
            .summarize(&SummaryRequest {
                title: &rec.title,
                duration_sec: rec.duration_sec,
                concept: &channel.concept,
                participants: &channel.participants,
                instruction: &rec.instruction,
                transcript: &rec.transcript,
            })
            .await?;

        let path = save_markdown(&self.summaries_dir, &channel, &rec, &summary)?;
        self.db.set_summary(id, &summary, &path.to_string_lossy())?;
        tracing::info!(recording_id = id, "요약 완료: {}", path.display());
        Ok(())
    }
}

/// 다운로드·저장용 마크다운 문서를 만든다.
pub fn markdown_document(channel: &Channel, rec: &Recording, summary: &str) -> String {
    let minutes = (rec.duration_sec / 60.0).round() as i64;
    let mut doc = format!(
        "# {}\n\n- 채널: {}\n- 녹음 일시: {}\n- 길이: 약 {}분\n",
        rec.title, channel.name, rec.created_at, minutes
    );
    if !rec.instruction.trim().is_empty() {
        doc.push_str(&format!("- 추가 요청: {}\n", rec.instruction.trim().replace('\n', " ")));
    }
    doc.push_str("\n---\n\n");
    doc.push_str(summary.trim());
    doc.push('\n');
    doc
}

/// `data/summaries/<채널>/<id>_<제목>.md`에 저장한다. 다시 요약하면 덮어쓴다.
fn save_markdown(dir: &Path, channel: &Channel, rec: &Recording, summary: &str) -> Result<PathBuf> {
    let channel_dir = dir.join(safe_file_name(&channel.name));
    std::fs::create_dir_all(&channel_dir)?;
    let path = channel_dir.join(format!("{:04}_{}.md", rec.id, safe_file_name(&rec.title)));
    std::fs::write(&path, markdown_document(channel, rec, summary))?;
    Ok(path)
}

/// 파일 이름에 쓸 수 없는 문자를 바꾸고 길이를 줄인다. 한글은 그대로 둔다.
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c })
        .take(60)
        .collect();
    let cleaned = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    if cleaned.is_empty() { "untitled".into() } else { cleaned }
}

#[cfg(test)]
mod tests {
    use super::safe_file_name;

    #[test]
    fn file_names_are_safe() {
        assert_eq!(safe_file_name("주간 회의: 1/2"), "주간 회의_ 1_2");
        assert_eq!(safe_file_name("  ..  "), "untitled");
        assert_eq!(safe_file_name("../../etc"), "_.._etc");
    }
}
