fn main() {
    let dst = cmake::build("cpp");
    println!("cargo:rustc-link-search=native={}/lib", dst.display());
    println!("cargo:rustc-link-lib=static=ac_cpp");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-lib=c++");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
    println!("cargo:rerun-if-changed=cpp/ac_cpp.cpp");
    println!("cargo:rerun-if-changed=cpp/CMakeLists.txt");
    println!("cargo:rerun-if-changed=cpp/include/onpair/search/byte_aho_corasick.h");
    println!("cargo:rerun-if-changed=cpp/include/onpair/search/aho_corasick_trie.h");
    println!("cargo:rerun-if-changed=../../contract/lb_candidate.h");
}
