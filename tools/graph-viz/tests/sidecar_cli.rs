use std::process::Command;

use onpair::search::index::build_token_frequency_index;
use onpair::{Column, Config, Dictionary};

#[test]
fn cli_builds_a_bundle_from_an_exact_sidecar() {
    let bytes = b"alphabetagammaalpha";
    let offsets = [0u32, 5, 9, 14, 19];
    let column = Column::compress(bytes, &offsets, Config::default()).unwrap();
    let frequencies = build_token_frequency_index(&column.codes, column.dict.num_tokens()).unwrap();
    let expected_fingerprint = onpair::fingerprint_text(onpair::mincut_fingerprint(
        column.dict.as_view(),
        &frequencies,
    ));

    let temporary = tempfile::tempdir().unwrap();
    let artifact = temporary.path().join("column.lbartifact");
    std::fs::write(
        &artifact,
        onpair::encode_sidecar(&column.dict, &frequencies),
    )
    .unwrap();
    let queries = temporary.path().join("queries.jsonl");
    std::fs::write(
        &queries,
        r#"{"id":"q1","op":"contains","needles":["alpha"]}
"#,
    )
    .unwrap();
    let bundle = temporary.path().join("bundle.json");

    let output = Command::new(env!("CARGO_BIN_EXE_onpair-graph-viz"))
        .args([
            "--artifact",
            artifact.to_str().unwrap(),
            "--queries",
            queries.to_str().unwrap(),
            "--bundle",
            bundle.to_str().unwrap(),
            "--no-measure",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(bundle).unwrap()).unwrap();
    assert_eq!(parsed["dictionary_fingerprint"], expected_fingerprint);
    assert_eq!(parsed["graphs"]["q1"].as_array().unwrap().len(), 1);
    assert!(parsed["graphs"]["q1"][0]["svg"].is_string());
}
