#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() {
    locmd::builder()
        .build(tauri::generate_context!())
        .expect("无法初始化 LocMd")
        .run(|_, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                locmd::shutdown();
            }
        })
}
