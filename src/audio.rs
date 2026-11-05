//! 오디오 파일을 Whisper 입력(16kHz 모노 f32)으로 바꾼다.
//!
//! 먼저 순수 Rust 디코더(symphonia)로 m4a(AAC/ALAC)·mp3·wav·flac·ogg를 처리하므로
//! ffmpeg가 없는 Windows·Mac에서도 동작한다. symphonia가 못 여는 형식(opus, webm 등)은
//! ffmpeg가 설치되어 있으면 ffmpeg로 처리한다.

use std::fs::File;
use std::path::Path;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::tools;

pub const SAMPLE_RATE: u32 = 16_000;

pub fn decode_to_16k_mono(path: &Path) -> Result<Vec<f32>> {
    match decode_with_symphonia(path) {
        Ok(samples) => Ok(samples),
        Err(sym_err) => match tools::find_executable("ffmpeg") {
            Some(ffmpeg) => decode_with_ffmpeg(&ffmpeg, path)
                .with_context(|| format!("내장 디코더 오류: {sym_err:#}")),
            None => Err(sym_err.context(
                "이 형식은 내장 디코더로 열 수 없습니다. m4a·mp3·wav·flac·ogg로 바꾸거나 ffmpeg를 설치하세요",
            )),
        },
    }
}

fn decode_with_symphonia(path: &Path) -> Result<Vec<f32>> {
    let file = File::open(path).with_context(|| format!("파일을 열 수 없습니다: {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .context("오디오 형식을 알아내지 못했습니다")?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .context("오디오 트랙이 없습니다")?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("이 오디오 코덱은 지원하지 않습니다")?;

    let mut mono: Vec<f32> = Vec::new();
    let mut source_rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(e).context("오디오를 읽는 중 오류"),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // 손상된 패킷 하나는 건너뛴다.
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(e).context("오디오 디코딩 오류"),
        };
        let spec = *decoded.spec();
        source_rate = spec.rate;
        let channels = spec.channels.count().max(1);
        let buf = sample_buf.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
        if buf.capacity() < decoded.capacity() * channels {
            *buf = SampleBuffer::new(decoded.capacity() as u64, spec);
        }
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks_exact(channels) {
            mono.push(frame.iter().sum::<f32>() / channels as f32);
        }
    }

    if mono.is_empty() || source_rate == 0 {
        bail!("오디오에 소리 데이터가 없습니다");
    }
    Ok(resample(&mono, source_rate, SAMPLE_RATE))
}

fn decode_with_ffmpeg(ffmpeg: &Path, path: &Path) -> Result<Vec<f32>> {
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args(["-ar", "16000", "-ac", "1", "-f", "f32le", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    tools::hide_console(&mut cmd);
    let output = cmd.output().context("ffmpeg를 실행하지 못했습니다")?;
    if !output.status.success() {
        bail!("ffmpeg 변환 실패: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let samples: Vec<f32> = output
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    if samples.is_empty() {
        bail!("오디오에 소리 데이터가 없습니다");
    }
    Ok(samples)
}

/// 윈도 씌운 sinc 보간으로 표본화 주파수를 바꾼다. 내려 바꿀 때는 새 나이퀴스트 주파수 위를
/// 걸러 내어 겹침(aliasing)을 막는다.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = to as f64 / from as f64;
    // 차단 주파수(입력 표본 기준, 0.5 = 입력 나이퀴스트). 약간 여유를 둔다.
    let cutoff = 0.5 * ratio.min(1.0) * 0.95;
    const ZERO_CROSSINGS: f64 = 16.0;
    let half_width = (ZERO_CROSSINGS / (2.0 * cutoff)).ceil() as isize;
    let out_len = ((input.len() as f64) * ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    let n = input.len() as isize;

    for i in 0..out_len {
        let t = i as f64 / ratio;
        let center = t.floor() as isize;
        let mut acc = 0.0f64;
        let mut norm = 0.0f64;
        for k in (center - half_width + 1)..=(center + half_width) {
            if k < 0 || k >= n {
                continue;
            }
            let x = t - k as f64;
            let w = window(x / half_width as f64);
            if w == 0.0 {
                continue;
            }
            let h = 2.0 * cutoff * sinc(2.0 * cutoff * x) * w;
            acc += input[k as usize] as f64 * h;
            norm += h;
        }
        out.push(if norm.abs() > 1e-9 { (acc / norm) as f32 } else { 0.0 });
    }
    out
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Blackman 창. |u| >= 1 이면 0.
fn window(u: f64) -> f64 {
    if u.abs() >= 1.0 {
        return 0.0;
    }
    let a = std::f64::consts::PI * (u + 1.0);
    0.42 - 0.5 * a.cos() + 0.08 * (2.0 * a).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: u32, secs: f64) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        (0..n).map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin() as f32).collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    #[test]
    fn keeps_speech_band() {
        // 1kHz 사인파는 48k → 16k로 바꿔도 크기가 유지된다(사인파 RMS ≈ 0.707).
        let out = resample(&sine(1000.0, 48_000, 1.0), 48_000, 16_000);
        assert_eq!(out.len(), 16_000);
        let r = rms(&out[1000..15_000]);
        assert!((r - 0.707).abs() < 0.02, "rms = {r}");
    }

    #[test]
    fn removes_above_nyquist() {
        // 12kHz는 16kHz 표본화로 표현할 수 없으므로 걸러져야 한다.
        let out = resample(&sine(12_000.0, 48_000, 1.0), 48_000, 16_000);
        let r = rms(&out[1000..15_000]);
        assert!(r < 0.02, "rms = {r}");
    }

    #[test]
    fn same_rate_is_identity() {
        let x = sine(440.0, 16_000, 0.1);
        assert_eq!(resample(&x, 16_000, 16_000), x);
    }
}
