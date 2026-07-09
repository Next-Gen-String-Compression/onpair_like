fn main() {
    // Always Release: the candidate's identity is its optimized build.
    let dst = cmake::Config::new("cpp").profile("Release").build();
    println!("cargo:rustc-link-search=native={}/lib", dst.display());
    println!("cargo:rustc-link-lib=static=lb_fsst");
    // C++ runtime: libc++ on Apple platforms, libstdc++ elsewhere.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-lib=c++");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
    println!("cargo:rerun-if-changed=cpp/fsst_candidate.cpp");
    println!("cargo:rerun-if-changed=cpp/CMakeLists.txt");
    println!("cargo:rerun-if-changed=../../contract/lb_candidate.h");
}
