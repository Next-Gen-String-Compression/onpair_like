fn main() {
    let dst = cmake::build("cpp");
    println!("cargo:rustc-link-search=native={}/lib", dst.display());
    println!("cargo:rustc-link-lib=static=lb_dict_uncompressed_prefilter");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-lib=c++");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
    println!("cargo:rerun-if-changed=cpp/dict_uncompressed_prefilter_candidate.cpp");
    println!("cargo:rerun-if-changed=cpp/CMakeLists.txt");
    println!("cargo:rerun-if-changed=../../contract/lb_candidate.h");
    println!("cargo:rerun-if-changed=../common/matcher/matcher.hpp");
    println!("cargo:rerun-if-changed=../common/matcher/uncompressed_matchers.hpp");
    println!("cargo:rerun-if-changed=../common/matcher/dict_matcher.hpp");
    println!("cargo:rerun-if-changed=../common/prefilter/prefilter.hpp");
}
