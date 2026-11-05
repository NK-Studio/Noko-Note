fn main() {
    // 앱 명령을 권한으로 등록해, capabilities에서 허용한 창에서만 부를 수 있게 한다.
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["save_markdown", "reveal_summary", "set_window_theme"])),
    )
    .expect("tauri build 설정 실패");
}
