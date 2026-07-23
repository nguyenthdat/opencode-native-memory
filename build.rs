fn main() {
    prost_build::Config::new()
        .compile_protos(&["schema/opencode/memory/v1/memory.proto"], &["schema"])
        .expect("compile Protobuf memory protocol schema");

    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target.as_str() {
        "macos" => {
            println!("cargo:rustc-link-arg-bin=opencode-memory=-Wl,-rpath,@loader_path/memory-libs")
        }
        "linux" => {
            println!("cargo:rustc-link-arg-bin=opencode-memory=-Wl,-rpath,$ORIGIN/memory-libs")
        }
        _ => {}
    }
    println!("cargo:rerun-if-changed=schema/opencode/memory/v1/memory.proto");
    println!("cargo:rerun-if-changed=build.rs");
}
