use std::collections::{BTreeMap, HashSet};

use super::*;

fn flatten(rows: &[Vec<u8>]) -> (Vec<u8>, Vec<u64>) {
    let mut data = Vec::new();
    let mut offsets = vec![0];
    for row in rows {
        data.extend(row);
        offsets.push(data.len() as u64);
    }
    (data, offsets)
}

fn exact_substrings(rows: &[Vec<u8>], max_len: usize) -> BTreeMap<Vec<u8>, u64> {
    let mut result = BTreeMap::new();
    for row in rows {
        let mut seen = HashSet::new();
        for i in 0..row.len() {
            for len in 1..=max_len.min(row.len() - i) {
                seen.insert(row[i..i + len].to_vec());
            }
        }
        for needle in seen {
            *result.entry(needle).or_default() += 1;
        }
    }
    result
}

fn check_catalogue(rows: &[Vec<u8>], max_len: usize) {
    let (payload, offsets) = flatten(rows);
    let index = SubstringIndex::new(
        &payload,
        &offsets,
        IndexLimits {
            max_needle_len: max_len,
            ..IndexLimits::default()
        },
    )
    .unwrap();
    let mut observed = BTreeMap::new();
    index.visit(|f| {
        for len in f.min_len..=f.max_len {
            let bytes = payload[f.position..f.position + len].to_vec();
            assert!(index.contains(&bytes));
            assert!(
                observed.insert(bytes, f.matching_rows).is_none(),
                "family overlap"
            );
        }
    });
    assert_eq!(observed, exact_substrings(rows, max_len), "rows={rows:?}");
}

#[test]
fn catalogue_equals_exhaustive_row_oracle() {
    let cases = [
        vec![vec![]],
        vec![vec![], vec![], vec![]],
        vec![
            b"apple apple".to_vec(),
            b"apple".to_vec(),
            vec![],
            b"pear".to_vec(),
        ],
        vec![b"appappapple".to_vec(), b"appapple".to_vec()],
        vec![b"aaaaaa".to_vec(), b"aaaaaa".to_vec(), b"aa".to_vec()],
        vec![vec![0, 255, 0, 128], vec![], vec![255, 0], vec![0]],
        vec![
            b"ab".to_vec(),
            b"cd".to_vec(),
            b"b".to_vec(),
            b"abc".to_vec(),
        ],
        vec![(0..=255).collect(), (0..=255).rev().collect()],
    ];
    for rows in cases {
        for max_len in [1, 4, 16, 256] {
            check_catalogue(&rows, max_len);
        }
    }
    let mut rng = Rng::from_seed(42);
    for _ in 0..150 {
        let rows: Vec<_> = (0..1 + rng.below(10))
            .map(|_| {
                (0..rng.below(24))
                    .map(|_| [0, b'a', b'b', 255][rng.below(4) as usize])
                    .collect()
            })
            .collect();
        check_catalogue(&rows, 12);
    }
}

#[test]
fn generation_is_unique_balanced_exact_and_reproducible() {
    let rows = vec![
        b"apple banana apple".to_vec(),
        b"apple banana".to_vec(),
        b"banana pear".to_vec(),
        vec![],
    ];
    let (payload, offsets) = flatten(&rows);
    let index = SubstringIndex::new(&payload, &offsets, IndexLimits::default()).unwrap();
    let mut request = BalancedRequest::new(rows.len() as u64, 7);
    request.negative_attempts = 200;
    let a = index.generate(&request).unwrap();
    let b = index.generate(&request).unwrap();
    assert_eq!(
        serde_json::to_vec(&a).unwrap(),
        serde_json::to_vec(&b).unwrap()
    );
    let oracle = exact_substrings(&rows, 256);
    let mut seen = HashSet::new();
    for n in &a.needles {
        assert!(seen.insert(n.bytes.clone()));
        assert_eq!(n.matching_rows, oracle.get(&n.bytes).copied().unwrap_or(0));
        let cell = &a.cells[n.cell];
        assert!((cell.length.min..=cell.length.max).contains(&n.bytes.len()));
        assert!((cell.matching_rows.min..=cell.matching_rows.max).contains(&n.matching_rows));
        if let Some(p) = n.mutation_position {
            let witness = &payload[n.source_position..n.source_position + n.bytes.len()];
            assert!(index.contains(witness));
            assert_eq!(
                witness.iter().zip(&n.bytes).filter(|(a, b)| a != b).count(),
                1
            );
            assert_ne!(witness[p], n.bytes[p]);
            assert_eq!(n.matching_rows, 0);
        }
    }
    for c in &a.cells {
        if let Some(available) = c.available {
            let exact = oracle
                .iter()
                .filter(|(n, count)| {
                    (c.length.min..=c.length.max).contains(&n.len())
                        && (c.matching_rows.min..=c.matching_rows.max).contains(count)
                })
                .count();
            assert_eq!(available, exact as u64);
            assert_eq!(c.generated, exact.min(request.per_cell));
        }
    }
    request.seed += 1;
    assert_ne!(
        serde_json::to_vec(&a.needles).unwrap(),
        serde_json::to_vec(&index.generate(&request).unwrap().needles).unwrap()
    );
}

#[test]
fn suite_names_identify_dataset_and_complete_generation_request() {
    let original = BalancedRequest::new(100, 42);
    let key = original.suite_key("dataset-a").unwrap();
    assert_eq!(key, original.suite_key("dataset-a").unwrap());
    assert_ne!(key, original.suite_key("dataset-b").unwrap());
    for field in 0..5 {
        let mut changed = original.clone();
        match field {
            0 => changed.seed += 1,
            1 => changed.per_cell += 1,
            2 => changed.negative_attempts += 1,
            3 => changed.lengths[0].max -= 1,
            _ => changed.matching_rows.pop().map(|_| ()).unwrap(),
        }
        assert_ne!(key, changed.suite_key("dataset-a").unwrap());
    }
}

#[test]
fn wide_length_buckets_and_negative_only_requests() {
    let rows = vec![(0..=255u8).cycle().take(512).collect()];
    let (payload, offsets) = flatten(&rows);
    let index = SubstringIndex::new(&payload, &offsets, IndexLimits::default()).unwrap();
    let mut request = BalancedRequest::new(1, 42);
    request.lengths = vec![LengthBucket { min: 129, max: 256 }];
    request.matching_rows = vec![RowBucket { min: 1, max: 1 }];
    let generated = index.generate(&request).unwrap();
    let lengths: HashSet<_> = generated.needles.iter().map(|n| n.bytes.len()).collect();
    assert_eq!(lengths.len(), 20);
    assert!(*lengths.iter().min().unwrap() <= 140);
    assert!(*lengths.iter().max().unwrap() >= 244);
    request.lengths = vec![LengthBucket { min: 2, max: 4 }];
    request.matching_rows = vec![RowBucket { min: 0, max: 0 }];
    let negatives = index.generate(&request).unwrap();
    assert_eq!(negatives.needles.len(), 20);
    assert!(negatives
        .needles
        .iter()
        .all(|n| !index.contains(&n.bytes) && n.mutation_position.is_some()));
}

#[test]
fn cache_roundtrip_corruption_and_dataset_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let (payload, offsets) = flatten(&[b"banana".to_vec(), vec![], b"anana".to_vec()]);
    let a = SubstringIndex::cached(&payload, &offsets, IndexLimits::default(), tmp.path()).unwrap();
    let b = SubstringIndex::cached(&payload, &offsets, IndexLimits::default(), tmp.path()).unwrap();
    let request = BalancedRequest::new(3, 4);
    assert_eq!(
        serde_json::to_vec(&a.generate(&request).unwrap()).unwrap(),
        serde_json::to_vec(&b.generate(&request).unwrap()).unwrap()
    );
    let other_offsets = [0, 5, 6, payload.len() as u64];
    SubstringIndex::cached(&payload, &other_offsets, IndexLimits::default(), tmp.path()).unwrap();
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 2);
    for entry in std::fs::read_dir(tmp.path()).unwrap() {
        let path = entry.unwrap().path();
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::write(path, bytes).unwrap();
    }
    assert!(
        SubstringIndex::cached(&payload, &offsets, IndexLimits::default(), tmp.path()).is_err()
    );
}

#[test]
fn boundaries_limits_and_impossible_cells_are_explicit() {
    assert_eq!(
        IndexLimits::required_memory_bytes(4, 2).unwrap(),
        64 * 1024 * 1024 + 96
    );
    assert!(IndexLimits::required_memory_bytes(i32::MAX as u64, 1).is_err());
    assert!(IndexLimits::required_memory_bytes(u64::MAX, 1).is_err());
    let (payload, offsets) = flatten(&[b"ab".to_vec(), b"cd".to_vec()]);
    let index = SubstringIndex::new(&payload, &offsets, IndexLimits::default()).unwrap();
    assert!(!index.contains(b"bc"));
    assert!(!index.contains(b"abcd"));
    assert!(SubstringIndex::new(&payload, &[0, 3, 2, 4], IndexLimits::default()).is_err());
    assert!(SubstringIndex::new(
        &payload,
        &offsets,
        IndexLimits {
            memory_budget_bytes: 1,
            ..IndexLimits::default()
        }
    )
    .is_err());
    let mut request = BalancedRequest::new(2, 0);
    request.lengths = vec![LengthBucket { min: 129, max: 256 }];
    let generated = index.generate(&request).unwrap();
    assert!(generated.needles.is_empty());
    assert_eq!(generated.cells[0].status, "unresolved");
    assert!(generated.cells[1..]
        .iter()
        .all(|c| c.status == "exhausted" && c.available == Some(0)));
}
