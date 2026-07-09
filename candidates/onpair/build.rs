fn main() {
    // Always Release: the candidate's identity is its optimized build
    // (compression is the measured artifact even under `cargo test`).
    let dst = cmake::Config::new("cpp").profile("Release").build();
    println!("cargo:rustc-link-search=native={}/lib", dst.display());
    println!("cargo:rustc-link-lib=static=lb_onpair");
    println!("cargo:rustc-link-lib=static=onpair");
    // C++ runtime: libc++ on Apple platforms, libstdc++ elsewhere.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-lib=c++");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
    println!("cargo:rerun-if-changed=cpp/onpair_candidate.cpp");
    println!("cargo:rerun-if-changed=cpp/CMakeLists.txt");
    println!("cargo:rerun-if-changed=../../contract/lb_candidate.h");
    // Local-checkout override for OnPair development (see cpp/CMakeLists.txt).
    println!("cargo:rerun-if-env-changed=ONPAIR_SOURCE_DIR");
}
