# Noko-Note

음성 파일을 올리면 **로컬 Whisper**로 받아쓰고, **채널(방)마다 정한 요약 컨셉**에 맞춰 **Claude 계정**으로 요약해 주는 로컬 웹 앱입니다. 요약은 화면에서 읽거나 md 파일로 저장하고, Slack·Discord로 보낼 수 있습니다.

```
음성 파일 ─▶ ffmpeg(16kHz 변환) ─▶ Whisper large-v3-turbo (Metal GPU, VAD)
          ─▶ 채널 컨셉 + 추가 요청 ─▶ Claude 계정(claude CLI) ─▶ 화면 / md 저장 / Slack·Discord
```

## 준비물

| 항목 | 설치 |
|---|---|
| Rust | `rustup` |
| ffmpeg | `brew install ffmpeg` |
| Claude Code CLI (claude.ai 계정 로그인) | 앱 화면의 **Claude 계정 연결** 버튼, 또는 `claude auth login --claudeai` |
| Whisper 모델 | `models/ggml-large-v3-turbo.bin`, `models/ggml-silero-v5.1.2.bin` (아래 명령) |

```bash
mkdir -p models
curl -L -o models/ggml-large-v3-turbo.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin
curl -L -o models/ggml-silero-v5.1.2.bin https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin
```

## 실행

```bash
cargo run --release
```

브라우저에서 http://127.0.0.1:3000 을 엽니다. 설정을 바꾸려면 `.env.example`을 `.env`로 복사해 고칩니다.

## 사용 흐름

1. 왼쪽에서 채널을 고르거나 **+ 새 채널**로 만듭니다.
2. 채널 설정에서 **요약 컨셉**을 적습니다. 예: "주간 회의록. 결정 사항과 담당자별 할 일을 표로."
3. 음성 파일을 끌어다 놓고 **올리고 요약하기**를 누릅니다. 이번 녹음에만 줄 추가 요청도 적을 수 있습니다.
4. 받아쓰기 → 요약이 끝나면 오른쪽에 요약이 나타납니다. **녹취록** 탭에서 원문을 볼 수 있습니다.
5. **다시 요약**으로 같은 녹취록을 다른 관점으로 요약할 수 있습니다(받아쓰기는 다시 하지 않음).
6. **md 다운로드**, **요약 복사**, **Slack/Discord 보내기**(채널 설정에 웹훅을 넣은 경우).

## 저장 위치

| 경로 | 내용 |
|---|---|
| `data/noko.db` | 채널·녹음·녹취록·요약 (SQLite) |
| `data/uploads/` | 올린 음성 파일 |
| `data/summaries/<채널>/<번호>_<제목>.md` | 요약 md 파일 (다시 요약하면 덮어씀) |

## Claude 계정 연동 방식

API 키 대신, 이 Mac에 claude.ai 계정으로 로그인된 Claude Code CLI를 비대화 모드로 호출합니다. 요약에 필요 없는 도구·MCP·스킬·사용자 설정은 모두 끄고 빈 작업 폴더(`data/claude-work`)에서 실행해 계정 사용량을 아낍니다. 요약은 구독(Pro/Max) 사용 한도 안에서 처리됩니다.

## 코드 구조

| 파일 | 역할 |
|---|---|
| `src/main.rs` | 설정, 웹 서버, API 라우트 |
| `src/pipeline.rs` | 받아쓰기 대기열 → 요약 → md 저장 |
| `src/transcribe.rs` | ffmpeg 디코딩 + whisper-rs 받아쓰기 |
| `src/summarize.rs` | Claude 계정(claude CLI) 요약, 로그인 상태 확인 |
| `src/notify.rs` | Slack·Discord 웹훅 전송 (새 메신저는 여기에 추가) |
| `src/db.rs` | SQLite 저장소 |
| `static/index.html` | 화면 |

## API

| 메서드 | 경로 | 설명 |
|---|---|---|
| GET | `/api/status` | Whisper 준비 여부, Claude 계정 로그인 상태 |
| POST | `/api/claude/login` | 브라우저에서 claude.ai 로그인 시작 |
| GET·POST | `/api/channels` | 채널 목록·생성 |
| GET·PUT·DELETE | `/api/channels/{id}` | 채널 조회·수정·삭제 |
| GET·POST | `/api/channels/{id}/recordings` | 녹음 목록·업로드(multipart: `file`, `title`, `instruction`) |
| GET·DELETE | `/api/recordings/{id}` | 녹음 상세·삭제 |
| POST | `/api/recordings/{id}/summarize` | 다시 요약 (`{"instruction": "..."}`) |
| GET | `/api/recordings/{id}/summary.md` | md 다운로드 |
| POST | `/api/recordings/{id}/send` | Slack·Discord 전송 (`{"target": "slack"}`) |

`transcribe.sh`는 앱 없이 터미널에서 바로 받아쓰기만 할 때 씁니다.
