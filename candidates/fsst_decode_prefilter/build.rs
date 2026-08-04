fn main() {
    let dst = cmake::Config::new("cpp").profile("Release").build();
    println!("cargo:rustc-link-search=native={}/lib", dst.display());
    println!("cargo:rustc-link-lib=static=lb_fsst_decode_prefilter");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-lib=c++");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
    println!("cargo:rerun-if-changed=cpp/fsst_decode_prefilter_candidate.cpp");
    println!("cargo:rerun-if-changed=cpp/CMakeLists.txt");
    println!("cargo:rerun-if-changed=cpp/localize.sh");
    println!("cargo:rerun-if-changed=../common/fsst_common/fsst_build.hpp");
    println!("cargo:rerun-if-changed=../common/matcher/matcher.hpp");
    println!("cargo:rerun-if-changed=../common/matcher/dict_matcher.hpp");
    println!("cargo:rerun-if-changed=../common/matcher/uncompressed_matchers.hpp");
    println!("cargo:rerun-if-changed=../common/prefilter/prefilter.hpp");
    println!("cargo:rerun-if-changed=../../contract/lb_candidate.h");
}
