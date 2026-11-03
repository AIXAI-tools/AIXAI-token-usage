fn main() {
    // tauri-winres 會把路徑裡的撇號跳脫成 \'，導致 RC.EXE 找不到 icon
    // （例：專案路徑含撇號時）。先把 icon 複製到 OUT_DIR（路徑不含撇號）再交給它。
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let icon = out.join("app-icon.ico");
    std::fs::copy("icons/icon.ico", &icon).expect("copy icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.ico");

    let attrs = tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new().window_icon_path(&icon));
    tauri_build::try_build(attrs).expect("tauri build script failed");
}
