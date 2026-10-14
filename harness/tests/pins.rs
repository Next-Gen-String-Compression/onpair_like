//! A git rev in a candidate's Cargo.toml has to be a literal; it must equal
//! `<CANDIDATE>_REV` in pins.env, where every other revision lives.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[test]
fn cargo_git_revs_match_pins_env() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let pins: HashMap<String, String> = fs::read_to_string(root.join("pins.env"))
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut checked = 0;
    for entry in fs::read_dir(root.join("candidates")).unwrap() {
        let dir = entry.unwrap().path();
        let Ok(manifest) = fs::read_to_string(dir.join("Cargo.toml")) else { continue };
        for line in manifest.lines().filter(|l| l.contains("git =")) {
            let Some(rev) = line.split("rev = \"").nth(1).and_then(|r| r.split('"').next()) else { continue };
            let key = format!("{}_REV", dir.file_name().unwrap().to_str().unwrap().to_uppercase());
            assert_eq!(pins.get(&key).map(String::as_str), Some(rev), "{}: {line}", dir.display());
            checked += 1;
        }
    }
    assert!(checked > 0, "no git rev found in candidates/*/Cargo.toml");
}
