fn main() {
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target.as_str() {
        "macos" => println!(
            "cargo:rustc-link-arg-bin=opencode-native-memory=-Wl,-rpath,@loader_path/native-memory-libs"
        ),
        "linux" => println!(
            "cargo:rustc-link-arg-bin=opencode-native-memory=-Wl,-rpath,$ORIGIN/native-memory-libs"
        ),
        _ => {}
    }
    println!("cargo:rerun-if-changed=build.rs");
}
