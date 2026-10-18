fn main() {
    let march = std::env::var("LB_FSST_MARCH").unwrap_or_else(|_| "native".into());
    let dst = cmake::Config::new("cpp")
        .profile("Release")
        .define("LB_FSST_MARCH", &march)
        .build();
    println!("cargo:rustc-link-search=native={}/lib", dst.display());
    println!("cargo:rustc-link-lib=static=lb_fsst_prefilter");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-lib=c++");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
    println!("cargo:rerun-if-changed=cpp/fsst_prefilter_candidate.cpp");
    println!("cargo:rerun-if-changed=cpp/CMakeLists.txt");
    println!("cargo:rerun-if-changed=cpp/localize.sh");
    println!("cargo:rerun-if-changed=../common/fsst_common/fsst_build.hpp");
    println!("cargo:rerun-if-changed=../common/matcher/matcher.hpp");
    println!("cargo:rerun-if-changed=../common/matcher/dict_matcher.hpp");
    println!("cargo:rerun-if-changed=../../contract/lb_candidate.h");
    // The FSST fork cpp/CMakeLists.txt builds from.
    println!("cargo:rerun-if-changed=../../third_party/fsst/search/prefilter");
    println!("cargo:rerun-if-changed=../../third_party/fsst/libfsst.cpp");
    println!("cargo:rerun-if-env-changed=LB_FSST_MARCH");
}
