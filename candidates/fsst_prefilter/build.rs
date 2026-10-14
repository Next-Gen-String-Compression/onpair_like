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
    println!("cargo:rerun-if-changed=../../pins.env");
    println!("cargo:rerun-if-changed=../common/pins.cmake");
    println!("cargo:rerun-if-changed=cpp/localize.sh");
    println!("cargo:rerun-if-changed=../common/fsst_common/fsst_build.hpp");
    println!("cargo:rerun-if-changed=../common/matcher/matcher.hpp");
    println!("cargo:rerun-if-changed=../common/matcher/dict_matcher.hpp");
    println!("cargo:rerun-if-changed=../../contract/lb_candidate.h");
    // The sibling checkout cpp/CMakeLists.txt builds from when present.
    println!("cargo:rerun-if-changed=../../../duckdb/third_party/fsst/upstream/search/prefilter");
    println!("cargo:rerun-if-changed=../../../duckdb/third_party/fsst/upstream/libfsst.cpp");
    println!("cargo:rerun-if-env-changed=FSST_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=LB_FSST_MARCH");
}
