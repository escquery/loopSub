fn main() {
    // tauri-build does not currently emit change tracking for bundle icons. Keep local
    // incremental builds in sync with the icon shown by Dock and Command-Tab.
    println!("cargo:rerun-if-changed=icons/icon.png");
    println!("cargo:rerun-if-changed=icons/icon.icns");
    println!("cargo:rerun-if-changed=icons/icon.ico");
    tauri_build::build()
}
