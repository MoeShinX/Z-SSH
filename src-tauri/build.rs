fn main() {
    // OneDrive 的 client ID 在构建时从这个环境变量编进程序（src/main.rs 里的 option_env!）。
    // 它变了就要重新编译，否则会继续用上一次编进去的值。
    println!("cargo:rerun-if-env-changed=ZSSH_ONEDRIVE_CLIENT_ID");
    tauri_build::build()
}
