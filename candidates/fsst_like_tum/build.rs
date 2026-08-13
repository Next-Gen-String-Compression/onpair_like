fn main() {
    // Always Release: the candidate's identity is its optimized build.
    let dst = cmake::Config::new("cpp").profile("Release").build();
    println!("cargo:rustc-link-search=native={}/lib", dst.display());
    println!("cargo:rustc-link-lib=static=lb_fsst_like_tum");
    // C++ runtime: libc++ on Apple platforms, libstdc++ elsewhere.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-lib=c++");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
    // The cpp codegen strategy dlopen()s its generated kernels. On glibc >= 2.34
    // dl* live in libc (libdl is an empty stub); older glibc needs the real lib.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-lib=dylib=dl");
    }
    // CMake writes lib/lb_llvm_link.txt only when it compiled the llvm backends
    // (HAVE_LLVM): line 1 = link-search dir, remaining lines = dylib names.
    // Linking from that file keeps compile-time HAVE_LLVM and the final harness
    // link in agreement by construction.
    if let Ok(link_info) = std::fs::read_to_string(dst.join("lib/lb_llvm_link.txt")) {
        let mut lines = link_info.lines().filter(|l| !l.trim().is_empty());
        if let Some(dir) = lines.next() {
            println!("cargo:rustc-link-search=native={}", dir.trim());
        }
        for lib in lines {
            println!("cargo:rustc-link-lib=dylib={}", lib.trim());
        }
    }
    println!("cargo:rerun-if-changed=cpp/fsst_like_tum_candidate.cpp");
    println!("cargo:rerun-if-changed=cpp/CMakeLists.txt");
    println!("cargo:rerun-if-changed=../common/fsst_common/fsst_build.hpp");
    println!("cargo:rerun-if-changed=../../contract/lb_candidate.h");
}
