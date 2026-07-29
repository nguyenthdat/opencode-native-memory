fn main() {
    prost_build::Config::new()
        .compile_protos(&["schema/opencode/memory/v1/memory.proto"], &["schema"])
        .expect("compile Protobuf memory protocol schema");

    prost_build::Config::new()
        .compile_protos(
            &["schema/opencode/memory/model/v1/model.proto"],
            &["schema"],
        )
        .expect("compile Protobuf model protocol schema");

    let mut daemon = prost_build::Config::new();
    daemon.extern_path(".opencode.memory.v1", "crate::memory_proto");
    daemon.extern_path(".opencode.memory.model.v1", "crate::model_proto");
    daemon
        .compile_protos(
            &["schema/opencode/memory/daemon/v1/daemon.proto"],
            &["schema"],
        )
        .expect("compile Protobuf daemon protocol schema");

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
    println!("cargo:rerun-if-changed=schema/opencode/memory/model/v1/model.proto");
    println!("cargo:rerun-if-changed=schema/opencode/memory/daemon/v1/daemon.proto");
    println!("cargo:rerun-if-changed=build.rs");
}
