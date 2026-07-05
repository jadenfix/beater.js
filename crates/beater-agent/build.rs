fn main() {
    cxx_build::bridge("src/cpp_bridge.rs")
        .file("src/cpp_tools.cc")
        .flag_if_supported("-std=c++17")
        .compile("beater-agent-cpp");
    println!("cargo:rerun-if-changed=src/cpp_bridge.rs");
    println!("cargo:rerun-if-changed=src/cpp_tools.cc");
    println!("cargo:rerun-if-changed=src/cpp_tools.h");
}
