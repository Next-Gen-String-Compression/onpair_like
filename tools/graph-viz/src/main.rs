//! Command-line front end: rows or a prepared benchmark dataset in, figures out.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use arrow_array::{Array, LargeBinaryArray};
use arrow_ipc::reader::FileReader;
use base64::Engine as _;
use onpair::search::index::build_token_frequency_index;
use onpair::{Column, Config, Dictionary, MaxDictBits, Threshold};
use onpair_graph_viz::{Options, ProbeWeight, visualize, visualize_index};
use serde::{Deserialize, Serialize};

const USAGE: &str = "\
onpair-graph-viz — render OnPair's prefilter alignment DAG and its minimum cut

USAGE:
    onpair-graph-viz --rows FILE --pattern STR [--pattern STR ...] [OPTIONS]
    onpair-graph-viz --dataset DIR --queries FILE --bundle FILE [OPTIONS]
    onpair-graph-viz --artifact FILE --queries FILE --bundle FILE --no-measure [OPTIONS]

INPUT:
    --rows FILE          Newline-delimited rows to train the dictionary on.
                         Repeatable; \"-\" reads standard input.
    --dataset DIR        Canonical benchmark dataset containing data.arrow.
    --artifact FILE      Exact dictionary/frequency sidecar exported by a
                         benchmark. Does not retrain or read the dataset.
    --pattern STR        Pattern to visualize. Repeatable.
    --pattern-hex HEX    Same, given as hex bytes (e.g. 2f696e646578).
    --queries FILE       Benchmark queries.jsonl; repeatable. Every needle is
                         rendered and associated with its query id.

OPTIONS:
    --out DIR            Where to write individual figures (default: out).
    --bundle FILE        Write one compact explorer bundle instead of files.
    --bits N             Dictionary budget in bits (default: 12).
    --threshold F        OnPair training threshold (default: 0.15).
    --seed N             Deterministic OnPair training seed (default: 42).
    --metric M           Cut objective: tf, tf_residual, df, df_residual
                         (default: tf). The df variants count rows and need a
                         pass over the column.
    --max-states N       Skip the SVG past N byte-offset states, keeping its
                         bundle error or individual JSON (default: 128; 0
                         disables the guard).
    --title T            Figure headline (default: input directory/file stem).
    --no-measure         Skip candidate/exact-row measurement.
    --gallery            Also write index.md linking every individual figure.
    -h, --help           Print this.
";

struct Args {
    rows: Vec<PathBuf>,
    dataset: Option<PathBuf>,
    artifact: Option<PathBuf>,
    patterns: Vec<Pattern>,
    query_files: Vec<PathBuf>,
    out: PathBuf,
    bundle: Option<PathBuf>,
    bits: u8,
    threshold: f64,
    seed: u64,
    metric: ProbeWeight,
    max_states: Option<usize>,
    title: Option<String>,
    measure: bool,
    gallery: bool,
}

#[derive(Clone, Debug)]
struct Pattern {
    query_id: Option<String>,
    needle_index: usize,
    bytes: Vec<u8>,
}

#[derive(Deserialize)]
struct CatalogQuery {
    id: String,
    op: String,
    needles: Vec<CatalogNeedle>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CatalogNeedle {
    Text(String),
    Binary { b64: String },
}

#[derive(Serialize)]
struct ExplorerBundle {
    version: u8,
    dictionary_bits: u8,
    dictionary_fingerprint: String,
    graphs: BTreeMap<String, Vec<ExplorerGraph>>,
}

#[derive(Serialize)]
struct ExplorerGraph {
    needle_index: usize,
    states: usize,
    cover_points: Option<usize>,
    cover_ranges: Option<usize>,
    comparison_cost: Option<usize>,
    covered_codes: Option<u64>,
    svg: Option<String>,
    error: Option<String>,
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args {
        rows: Vec::new(),
        dataset: None,
        artifact: None,
        patterns: Vec::new(),
        query_files: Vec::new(),
        out: PathBuf::from("out"),
        bundle: None,
        bits: 12,
        threshold: 0.15,
        seed: 42,
        metric: ProbeWeight::TermFrequency,
        max_states: Some(onpair_graph_viz::DEFAULT_MAX_STATES),
        title: None,
        measure: true,
        gallery: false,
    };

    let mut raw = std::env::args().skip(1);
    while let Some(flag) = raw.next() {
        let mut value = |flag: &str| raw.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--rows" => args.rows.push(PathBuf::from(value(&flag)?)),
            "--dataset" => args.dataset = Some(PathBuf::from(value(&flag)?)),
            "--artifact" => args.artifact = Some(PathBuf::from(value(&flag)?)),
            "--pattern" => args.patterns.push(Pattern {
                query_id: None,
                needle_index: 0,
                bytes: value(&flag)?.into_bytes(),
            }),
            "--pattern-hex" => args.patterns.push(Pattern {
                query_id: None,
                needle_index: 0,
                bytes: parse_hex(&value(&flag)?)?,
            }),
            "--queries" => args.query_files.push(PathBuf::from(value(&flag)?)),
            "--out" => args.out = PathBuf::from(value(&flag)?),
            "--bundle" => args.bundle = Some(PathBuf::from(value(&flag)?)),
            "--bits" => {
                args.bits = value(&flag)?
                    .parse()
                    .map_err(|_| "--bits needs an integer from 9 through 16".to_string())?;
                MaxDictBits::new(args.bits)
                    .map_err(|_| "--bits needs an integer from 9 through 16".to_string())?;
            }
            "--threshold" => {
                args.threshold = value(&flag)?
                    .parse()
                    .map_err(|_| "--threshold needs a number in (0, 1]".to_string())?;
                Threshold::new(args.threshold)
                    .map_err(|_| "--threshold needs a number in (0, 1]".to_string())?;
            }
            "--seed" => {
                args.seed = value(&flag)?
                    .parse()
                    .map_err(|_| "--seed needs an unsigned integer".to_string())?;
            }
            "--metric" => {
                let text = value(&flag)?;
                args.metric =
                    ProbeWeight::parse(&text).ok_or_else(|| format!("unknown metric {text:?}"))?;
            }
            "--max-states" => {
                let count: usize = value(&flag)?
                    .parse()
                    .map_err(|_| "--max-states needs a number".to_string())?;
                args.max_states = (count > 0).then_some(count);
            }
            "--title" => args.title = Some(value(&flag)?),
            "--no-measure" => args.measure = false,
            "--gallery" => args.gallery = true,
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }

    let inputs = usize::from(!args.rows.is_empty())
        + usize::from(args.dataset.is_some())
        + usize::from(args.artifact.is_some());
    if inputs != 1 {
        return Err("provide exactly one of --rows, --dataset, or --artifact".to_string());
    }
    if args.artifact.is_some() && args.measure {
        return Err(
            "--artifact requires --no-measure (the sidecar intentionally has no rows)".into(),
        );
    }
    for path in &args.query_files {
        args.patterns.extend(read_query_patterns(path)?);
    }
    if args.patterns.is_empty() {
        return Err("at least one --pattern or --queries file is required".to_string());
    }
    if args.bundle.is_some()
        && args
            .patterns
            .iter()
            .any(|pattern| pattern.query_id.is_none())
    {
        return Err(
            "--bundle requires patterns from --queries so every graph has an id".to_string(),
        );
    }
    Ok(Some(args))
}

fn read_query_patterns(path: &Path) -> Result<Vec<Pattern>, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut patterns = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let query: CatalogQuery = serde_json::from_str(line)
            .map_err(|error| format!("{}:{}: {error}", path.display(), line_index + 1))?;
        if !matches!(
            query.op.as_str(),
            "contains" | "multi_contains" | "contains_any"
        ) {
            continue;
        }
        for (needle_index, needle) in query.needles.into_iter().enumerate() {
            let bytes = match needle {
                CatalogNeedle::Text(text) => text.into_bytes(),
                CatalogNeedle::Binary { b64 } => base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|error| {
                        format!(
                            "{}:{}: invalid base64 needle: {error}",
                            path.display(),
                            line_index + 1
                        )
                    })?,
            };
            patterns.push(Pattern {
                query_id: Some(query.id.clone()),
                needle_index,
                bytes,
            });
        }
    }
    Ok(patterns)
}

fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if !cleaned.len().is_multiple_of(2) {
        return Err("--pattern-hex needs an even number of digits".to_string());
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&cleaned[index..index + 2], 16)
                .map_err(|_| format!("{:?} is not hex", &cleaned[index..index + 2]))
        })
        .collect()
}

/// One row per line, trailing newline dropped. Empty trailing line ignored.
fn read_rows(paths: &[PathBuf]) -> Result<(Vec<u8>, Vec<u64>), String> {
    let mut bytes = Vec::new();
    let mut offsets = vec![0u64];
    for path in paths {
        let text = if path.as_os_str() == "-" {
            let mut buffer = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buffer)
                .map_err(|error| format!("stdin: {error}"))?;
            buffer
        } else {
            fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?
        };
        for line in text.split(|&byte| byte == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                continue;
            }
            bytes.extend_from_slice(line);
            offsets.push(bytes.len() as u64);
        }
    }
    if offsets.len() < 2 {
        return Err("no rows found in the input".to_string());
    }
    Ok((bytes, offsets))
}

fn read_dataset(path: &Path) -> Result<(Vec<u8>, Vec<u64>), String> {
    let arrow_path = path.join("data.arrow");
    let file = fs::File::open(&arrow_path)
        .map_err(|error| format!("{}: {error}", arrow_path.display()))?;
    let mut reader = FileReader::try_new(file, None)
        .map_err(|error| format!("{}: {error}", arrow_path.display()))?;
    let batch = reader
        .next()
        .ok_or_else(|| format!("{}: no record batch", arrow_path.display()))?
        .map_err(|error| format!("{}: {error}", arrow_path.display()))?;
    if reader.next().is_some() || batch.num_columns() != 1 {
        return Err(format!(
            "{} must contain one batch and one column",
            arrow_path.display()
        ));
    }
    let array = batch
        .column(0)
        .as_any()
        .downcast_ref::<LargeBinaryArray>()
        .ok_or_else(|| {
            format!(
                "{} must contain one LargeBinary column",
                arrow_path.display()
            )
        })?;
    let offsets = array
        .value_offsets()
        .iter()
        .map(|&offset| {
            u64::try_from(offset)
                .map_err(|_| format!("{} has a negative offset", arrow_path.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let end = *offsets
        .last()
        .ok_or_else(|| format!("{} has no offsets", arrow_path.display()))? as usize;
    if end > array.values().len() {
        return Err(format!(
            "{} has an offset beyond its payload",
            arrow_path.display()
        ));
    }
    Ok((array.values()[..end].to_vec(), offsets))
}

fn slug(pattern: &[u8]) -> String {
    let mut out = String::new();
    for &byte in pattern.iter().take(48) {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' => out.push(byte as char),
            b'A'..=b'Z' => out.push(byte.to_ascii_lowercase() as char),
            _ if out.ends_with('-') => {}
            _ => out.push('-'),
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        format!("pattern-{}b", pattern.len())
    } else {
        trimmed
    }
}

fn run() -> Result<(), String> {
    let Some(args) = parse_args()? else {
        print!("{USAGE}");
        return Ok(());
    };

    enum Source {
        Column {
            column: Column<u64>,
            frequencies: onpair::search::index::TokenFrequencyIndex,
        },
        Artifact(onpair::Artifact),
    }
    let source = if let Some(path) = &args.artifact {
        let bytes = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        Source::Artifact(
            onpair::decode_sidecar(&bytes)
                .map_err(|error| format!("{}: {error}", path.display()))?,
        )
    } else {
        let (bytes, offsets) = match &args.dataset {
            Some(path) => read_dataset(path)?,
            None => read_rows(&args.rows)?,
        };
        let config = Config {
            max_dict_bits: MaxDictBits::new(args.bits).expect("validated by parse_args"),
            threshold: Threshold::new(args.threshold).expect("validated by parse_args"),
            seed: Some(args.seed),
        };
        let column = Column::compress(&bytes, &offsets, config)
            .map_err(|error| format!("compression failed: {error}"))?;
        let frequencies = build_token_frequency_index(&column.codes, column.dict.num_tokens())
            .map_err(|error| format!("frequency index: {error}"))?;
        Source::Column {
            column,
            frequencies,
        }
    };
    let (dictionary_bits, dictionary_fingerprint, num_rows, num_codes, num_tokens) = match &source {
        Source::Column {
            column,
            frequencies,
        } => (
            args.bits,
            onpair::fingerprint_text(onpair::mincut_fingerprint(
                column.dict.as_view(),
                frequencies,
            )),
            column.view().num_rows(),
            column.codes.len(),
            column.dict.num_tokens(),
        ),
        Source::Artifact(artifact) => (
            artifact.dictionary_bits,
            onpair::fingerprint_text(artifact.fingerprint),
            0,
            artifact.indexed_codes as usize,
            artifact.dictionary.num_tokens(),
        ),
    };

    let title = args.title.clone().unwrap_or_else(|| {
        args.dataset
            .as_ref()
            .or(args.artifact.as_ref())
            .or_else(|| args.rows.first())
            .and_then(|path| path.file_stem())
            .map_or_else(
                || "column".to_string(),
                |stem| stem.to_string_lossy().into_owned(),
            )
    });
    println!(
        "column: {} rows, {} codes, {} dictionary tokens ({}-bit budget)",
        num_rows, num_codes, num_tokens, dictionary_bits
    );

    if args.bundle.is_none() {
        fs::create_dir_all(&args.out)
            .map_err(|error| format!("{}: {error}", args.out.display()))?;
    }
    let mut gallery = BTreeMap::new();
    let mut bundle_graphs: BTreeMap<String, Vec<ExplorerGraph>> = BTreeMap::new();
    let mut unsound = 0usize;

    for (index, pattern) in args.patterns.iter().enumerate() {
        let options = Options {
            metric: args.metric,
            max_states: args.max_states,
            title: title.clone(),
            subtitle: None,
            measure: args.measure,
        };
        let rendered = match &source {
            Source::Column {
                column,
                frequencies,
            } => visualize(column.view(), frequencies, &pattern.bytes, &options),
            Source::Artifact(artifact) => visualize_index(
                artifact.dictionary.as_view(),
                &artifact.frequencies,
                &pattern.bytes,
                &options,
            ),
        };
        let figure = match rendered {
            Ok(figure) => figure,
            Err(error) if args.bundle.is_some() => {
                let query_id = pattern
                    .query_id
                    .as_ref()
                    .expect("bundle patterns have query ids");
                bundle_graphs
                    .entry(query_id.clone())
                    .or_default()
                    .push(ExplorerGraph {
                        needle_index: pattern.needle_index,
                        states: 0,
                        cover_points: None,
                        cover_ranges: None,
                        comparison_cost: None,
                        covered_codes: None,
                        svg: None,
                        error: Some(error.to_string()),
                    });
                if (index + 1) % 100 == 0 || index + 1 == args.patterns.len() {
                    println!(
                        "  rendered {}/{} query needles",
                        index + 1,
                        args.patterns.len()
                    );
                }
                continue;
            }
            Err(error) => {
                return Err(format!(
                    "{}: {error}",
                    String::from_utf8_lossy(&pattern.bytes)
                ));
            }
        };

        if figure
            .measurement
            .as_ref()
            .is_some_and(|measurement| !measurement.sound)
        {
            unsound += 1;
        }

        if let Some(query_id) = &pattern.query_id {
            let error = figure.svg.is_none().then(|| {
                format!(
                    "graph has {} states, above the --max-states limit",
                    figure.states()
                )
            });
            bundle_graphs
                .entry(query_id.clone())
                .or_default()
                .push(ExplorerGraph {
                    needle_index: pattern.needle_index,
                    states: figure.states(),
                    cover_points: Some(figure.cover.points),
                    cover_ranges: Some(figure.cover.ranges),
                    comparison_cost: Some(figure.cover.cmp_cost),
                    covered_codes: Some(figure.summary.cover_frequency),
                    svg: figure.svg.clone(),
                    error,
                });
        }

        if args.bundle.is_some() {
            if (index + 1) % 100 == 0 || index + 1 == args.patterns.len() {
                println!(
                    "  rendered {}/{} query needles",
                    index + 1,
                    args.patterns.len()
                );
            }
            continue;
        }

        let stem = format!("{:02}-{}", index + 1, slug(&pattern.bytes));
        let json_path = args.out.join(format!("{stem}.json"));
        fs::write(
            &json_path,
            serde_json::to_vec_pretty(&figure)
                .map_err(|error| format!("serializing {stem}: {error}"))?,
        )
        .map_err(|error| format!("{}: {error}", json_path.display()))?;

        let svg_name = match &figure.svg {
            Some(svg) => {
                let svg_path = args.out.join(format!("{stem}.svg"));
                fs::write(&svg_path, svg)
                    .map_err(|error| format!("{}: {error}", svg_path.display()))?;
                Some(format!("{stem}.svg"))
            }
            None => {
                println!(
                    "  skipped SVG: {} states exceeds --max-states (JSON written)",
                    figure.states()
                );
                None
            }
        };

        let measured = figure.measurement.as_ref().map_or_else(
            || "not measured".to_string(),
            |measurement| {
                let theirs = measurement.onpair_candidates.map_or_else(
                    || {
                        measurement
                            .onpair_refusal
                            .clone()
                            .unwrap_or_else(|| "n/a".to_string())
                    },
                    |count| count.to_string(),
                );
                format!(
                    "{} candidates ({} exact), onpair {theirs}, sound {}",
                    measurement.candidates, measurement.exact_rows, measurement.sound
                )
            },
        );
        let pruned = if figure.dead_probes.is_empty() {
            String::new()
        } else {
            format!(" ({} pruned as never used)", figure.dead_probes.len())
        };
        println!(
            "  {stem}: {} alignments, cut {} probes{pruned} weight {} · cover {} comparisons ({} whole-needle) · {measured}",
            figure.graph.stats.feasible_alignments,
            figure.cut.selected_nodes.len(),
            figure.cut.value,
            figure.cover.cmp_cost,
            figure.graph.contained.len()
        );

        if let Some(svg_name) = svg_name {
            gallery.insert(
                stem,
                (
                    String::from_utf8_lossy(&pattern.bytes).into_owned(),
                    svg_name,
                ),
            );
        }
    }

    if let Some(path) = &args.bundle {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        let bundle = ExplorerBundle {
            version: 1,
            dictionary_bits,
            dictionary_fingerprint,
            graphs: bundle_graphs,
        };
        fs::write(
            path,
            serde_json::to_vec(&bundle)
                .map_err(|error| format!("serializing {}: {error}", path.display()))?,
        )
        .map_err(|error| format!("{}: {error}", path.display()))?;
        println!("bundle: {}", path.display());
    }

    if args.gallery {
        let mut index = String::from("# Min-cut graph gallery\n");
        for (stem, (pattern, svg)) in &gallery {
            let _ = write!(
                index,
                "\n## `{pattern}`\n\n![{pattern}]({svg})\n\n[Graph JSON]({stem}.json)\n"
            );
        }
        let path = args.out.join("index.md");
        fs::write(&path, index).map_err(|error| format!("{}: {error}", path.display()))?;
        println!("gallery: {}", path.display());
    }

    if unsound > 0 {
        return Err(format!(
            "{unsound} cover(s) missed a true match — the rendered graph is wrong"
        ));
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
