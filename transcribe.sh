#!/usr/bin/env bash
# 로컬 Whisper(whisper.cpp)로 오디오 파일을 받아쓰기한다.
# 사용법: ./transcribe.sh <오디오파일> [언어=ko]
# 결과: 같은 폴더에 <파일명>.txt, <파일명>.srt 생성
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
MODEL="$DIR/models/ggml-large-v3-turbo.bin"
VAD_MODEL="$DIR/models/ggml-silero-v5.1.2.bin"

IN="${1:?사용법: $0 <오디오파일> [언어=ko]}"
LANG_CODE="${2:-ko}"
OUT="${IN%.*}"

TMP_WAV="$(mktemp -t whisper).wav"
trap 'rm -f "$TMP_WAV"' EXIT

# whisper.cpp는 16kHz 모노 WAV를 입력으로 받는다.
ffmpeg -y -v error -i "$IN" -ar 16000 -ac 1 -c:a pcm_s16le "$TMP_WAV"

whisper-cli \
  -m "$MODEL" \
  -l "$LANG_CODE" \
  --vad -vm "$VAD_MODEL" \
  -otxt -osrt -of "$OUT" \
  -f "$TMP_WAV"

echo "완료: $OUT.txt, $OUT.srt"
