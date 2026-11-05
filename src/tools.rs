//! 외부 실행 파일(claude, ffmpeg) 찾기와 실행 보조.
//!
//! Finder·시작 메뉴로 연 데스크톱 앱은 터미널의 PATH를 물려받지 못한다. 그래서 PATH 외에도
//! 흔히 설치되는 위치와 (Mac·Linux) 로그인 셸의 PATH까지 찾아본다.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// `NOKO_<NAME>_BIN` 환경 변수 → PATH → 흔한 설치 위치 → 로그인 셸 순으로 찾는다.
pub fn find_executable(name: &str) -> Option<PathBuf> {
    let env_key = format!("NOKO_{}_BIN", name.to_uppercase());
    if let Some(p) = std::env::var_os(&env_key).map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(common_dirs());
    dirs.extend(login_shell_path());

    for dir in &dirs {
        for candidate in candidate_names(name) {
            let p = dir.join(&candidate);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn candidate_names(name: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{name}.exe"), format!("{name}.cmd"), format!("{name}.bat")]
    } else {
        vec![name.to_string()]
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

fn common_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(h) = home() {
        dirs.push(h.join(".local").join("bin"));
        dirs.push(h.join(".claude").join("local"));
        dirs.push(h.join(".npm-global").join("bin"));
        dirs.push(h.join(".bun").join("bin"));
        dirs.push(h.join(".volta").join("bin"));
        dirs.push(h.join("bin"));
    }
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(appdata).join("npm"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let local = PathBuf::from(local);
            dirs.push(local.join("Programs").join("claude"));
            dirs.push(local.join("Microsoft").join("WinGet").join("Links"));
        }
        if let Some(pf) = std::env::var_os("ProgramFiles") {
            dirs.push(PathBuf::from(pf).join("ffmpeg").join("bin"));
        }
        dirs.push(PathBuf::from(r"C:\ffmpeg\bin"));
        if let Some(h) = home() {
            dirs.push(h.join("scoop").join("shims"));
        }
        dirs.push(PathBuf::from(r"C:\ProgramData\chocolatey\bin"));
    } else {
        dirs.push("/opt/homebrew/bin".into());
        dirs.push("/usr/local/bin".into());
        dirs.push("/usr/bin".into());
    }
    dirs
}

/// 로그인 셸이 쓰는 PATH(nvm, asdf 등이 설정한 경로 포함). 한 번만 구한다.
fn login_shell_path() -> Vec<PathBuf> {
    static CACHE: OnceLock<Vec<PathBuf>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            if cfg!(windows) {
                return Vec::new();
            }
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
            let output = std::process::Command::new(shell)
                .args(["-lc", "printf %s \"$PATH\""])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
            match output {
                Ok(o) if o.status.success() => {
                    std::env::split_paths(&String::from_utf8_lossy(&o.stdout).to_string()).collect()
                }
                _ => Vec::new(),
            }
        })
        .clone()
}

/// 자식 프로세스의 PATH 앞에 실행 파일이 있는 폴더를 넣는다(그 프로그램이 같은 폴더의
/// 다른 도구를 부를 때를 대비).
pub fn path_with(dir: &Path) -> std::ffi::OsString {
    let mut dirs = vec![dir.to_path_buf()];
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

/// Windows에서 자식 프로세스가 콘솔 창을 띄우지 않게 한다.
pub fn hide_console(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// tokio 프로세스용 `hide_console`.
pub fn hide_console_tokio(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}
