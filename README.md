# Noko-Note

음성 파일을 올리면 **로컬 Whisper**로 받아쓰고, **채널(방)마다 정한 요약 컨셉**에 맞춰 **Claude 계정**으로 요약해 주는 로컬 웹 앱입니다. 요약은 화면에서 읽거나 md 파일로 저장하고, Slack·Discord로 보낼 수 있습니다.

**웹 버전**(브라우저)과 **데스크톱 앱**(Tauri 2, Mac·Windows) 두 가지로 쓸 수 있고, 화면과 기능은 같습니다.

```
음성 파일 ─▶ 내장 디코더(16kHz 변환) ─▶ Whisper large-v3-turbo (Mac: Metal GPU, VAD)
          ─▶ 채널 컨셉 + 추가 요청 ─▶ Claude 계정(claude CLI) ─▶ 화면 / md 저장 / Slack·Discord
```

## 준비물

| 항목 | 설치 |
|---|---|
| Rust | `rustup` |
| ffmpeg (선택) | m4a·mp3·wav·flac·ogg는 내장 디코더로 처리. opus·webm 등만 ffmpeg 필요 |
| Claude Code CLI (claude.ai 계정 로그인) | 앱 화면의 **Claude 계정 연결** 버튼, 또는 `claude auth login --claudeai` |
| Whisper 모델 | 화면의 **모델 내려받기** 버튼, 또는 아래 명령 |

```bash
mkdir -p models
curl -L -o models/ggml-large-v3-turbo.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin
curl -L -o models/ggml-silero-v5.1.2.bin https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin
```

## 실행 — 웹 버전

```bash
cargo run --release
```

브라우저에서 http://127.0.0.1:3000 을 엽니다. 설정을 바꾸려면 `.env.example`을 `.env`로 복사해 고칩니다.

## 실행 — 데스크톱 앱 (Tauri 2)

```bash
cargo install tauri-cli --version 2.12.4 --locked   # 처음 한 번
cd src-tauri
cargo tauri dev          # 개발 실행
cargo tauri build        # 설치 파일 만들기
```

- Mac 결과물: `target/release/bundle/macos/Noko-Note.app`, `bundle/dmg/*.dmg`
- Windows 결과물: `target/release/bundle/msi/*.msi`, `bundle/nsis/*-setup.exe`
- Windows 설치 파일은 Windows PC에서 빌드하거나, GitHub에 올린 뒤 **Actions → desktop → Run workflow**로 만듭니다(`.github/workflows/desktop.yml`, Mac arm64·Intel·Windows 동시 빌드).
- Windows에서 직접 빌드하려면 Visual Studio Build Tools(C++), CMake, LLVM(`LIBCLANG_PATH`)이 필요합니다.

데스크톱 앱은 앱 안에서 로컬 서버를 빈 포트로 띄우고 그 화면을 창에 엽니다. 웹 버전과 데이터는 따로 저장됩니다.

| | Mac | Windows |
|---|---|---|
| 데이터·모델 | `~/Library/Application Support/com.noko.note/` | `%APPDATA%\com.noko.note\` |
| 로그 | `~/Library/Logs/com.noko.note/noko-note.log` | `%LOCALAPPDATA%\com.noko.note\logs\` |
| 받아쓰기 | Metal GPU | CPU (가벼운 `q5_0` 모델 권장) |

데스크톱에서만 되는 기능: **md로 저장…**(저장 대화상자), **Finder/탐색기에서 보기**.

> 코드 서명을 하지 않은 빌드라 다른 컴퓨터에서 처음 열 때 경고가 뜹니다. Mac은 앱을 오른쪽 클릭 → 열기, Windows는 SmartScreen에서 "추가 정보 → 실행"을 누릅니다.

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
| `src/lib.rs` | 설정(`Config`), 모듈 묶음 |
| `src/main.rs` | 웹 버전 실행 파일 |
| `src/server.rs` | 웹 서버, API 라우트, 외부 사이트 요청 차단 |
| `src/models.rs` | Whisper 모델 확인·내려받기·불러오기 |
| `src/audio.rs` | 오디오 디코딩(symphonia) + 16kHz 리샘플링, ffmpeg 대체 경로 |
| `src/tools.rs` | `claude`·`ffmpeg` 실행 파일 찾기(앱에서 PATH가 없을 때 대비) |
| `src-tauri/` | Tauri 2 데스크톱 앱(창, md 저장, 파일 위치 열기) |
| `src/pipeline.rs` | 받아쓰기 대기열 → 요약 → md 저장 |
| `src/transcribe.rs` | whisper-rs 받아쓰기 |
| `src/summarize.rs` | Claude 계정(claude CLI) 요약, 로그인 상태 확인 |
| `src/notify.rs` | Slack·Discord 웹훅 전송 (새 메신저는 여기에 추가) |
| `src/db.rs` | SQLite 저장소 |
| `static/index.html` | 화면 |

## API

| 메서드 | 경로 | 설명 |
|---|---|---|
| GET | `/api/status` | Whisper 모델 상태, Claude 계정 로그인 상태, 요약 모델 목록·현재 모델 |
| PUT | `/api/settings/summary-model` | 요약 모델 바꾸기 (`{"model": "claude-opus-5-5"}`) |
| POST | `/api/whisper/download` | 모델 내려받기 (`{"model": "large-v3-turbo"}`) |
| POST | `/api/claude/login` | 브라우저에서 claude.ai 로그인 시작 |
| GET·POST | `/api/channels` | 채널 목록·생성 |
| GET·PUT·DELETE | `/api/channels/{id}` | 채널 조회·수정·삭제 |
| GET·POST | `/api/channels/{id}/recordings` | 녹음 목록·업로드(multipart: `file`, `title`, `instruction`) |
| GET·DELETE | `/api/recordings/{id}` | 녹음 상세·삭제 |
| POST | `/api/recordings/{id}/summarize` | 다시 요약 (`{"instruction": "..."}`), 받아쓰기 실패 건은 받아쓰기부터 |
| GET | `/api/recordings/{id}/summary.md` | md 다운로드 |
| POST | `/api/recordings/{id}/send` | Slack·Discord 전송 (`{"target": "slack"}`) |

`transcribe.sh`는 앱 없이 터미널에서 바로 받아쓰기만 할 때 씁니다.
