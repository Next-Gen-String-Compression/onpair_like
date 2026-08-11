//! Command-line front end: rows in, figures out.
//!
//! The library takes a column and a frequency index; this binary is the small
//! amount of glue that turns a text file into one. Nothing here knows about
//! datasets or benchmarks — point it at any newline-delimited file.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use onpair::search::build_token_frequency_index;
use onpair::{Column, DEFAULT_CONFIG, DictionaryView};
use onpair_graph_viz::{Options, ProbeWeight, visualize};

const USAGE: &str = "\
onpair-graph-viz — render OnPair's prefilter alignment DAG and its minimum cut

USAGE:
    onpair-graph-viz --rows FILE --pattern STR [--pattern STR ...] [OPTIONS]

INPUT:
    --rows FILE          Newline-delimited rows to train the dictionary on.
                         Repeatable; \"-\" reads standard input.
    --pattern STR        Pattern to visualize. Repeatable.
    --pattern-hex HEX    Same, given as hex bytes (e.g. 2f696e646578).

OPTIONS:
    --out DIR            Where to write figures (default: out).
    --metric M           Cut objective: tf, tf_residual, df, df_residual
                         (default: tf). The df variants count rows and need a
                         pass over the column.
    --max-states N       Skip the SVG past N byte-offset states, keeping the
                         JSON (default: 128; 0 disables the guard).
    --title T            Figure headline (default: the rows file stem).
    --no-measure         Skip candidate/exact-row measurement.
    --gallery            Also write index.md linking every figure.
    -h, --help           Print this.
";

struct Args {
    rows: Vec<PathBuf>,
    patterns: Vec<Vec<u8>>,
    out: PathBuf,
    metric: ProbeWeight,
    max_states: Option<usize>,
    title: Option<String>,
    measure: bool,
    gallery: bool,
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args {
        rows: Vec::new(),
        patterns: Vec::new(),
        out: PathBuf::from("out"),
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
            "--pattern" => args.patterns.push(value(&flag)?.into_bytes()),
            "--pattern-hex" => args.patterns.push(parse_hex(&value(&flag)?)?),
            "--out" => args.out = PathBuf::from(value(&flag)?),
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

    if args.rows.is_empty() {
        return Err("--rows is required".to_string());
    }
    if args.patterns.is_empty() {
        return Err("at least one --pattern is required".to_string());
    }
    Ok(Some(args))
}

fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if !cleaned.len().is_multiple_of(2) {
        return Err("--pattern-hex needs an even number of digits".to_string());
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&cleaned[i..i + 2], 16)
                .map_err(|_| format!("{:?} is not hex", &cleaned[i..i + 2]))
        })
        .collect()
}

/// One row per line, trailing newline dropped. Empty trailing line ignored.
fn read_rows(paths: &[PathBuf]) -> Result<(Vec<u8>, Vec<u32>), String> {
    let mut bytes = Vec::new();
    let mut offsets = vec![0u32];
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
            let end = u32::try_from(bytes.len())
                .map_err(|_| "input exceeds 4 GiB of rows".to_string())?;
            offsets.push(end);
        }
    }
    if offsets.len() < 2 {
        return Err("no rows found in the input".to_string());
    }
    Ok((bytes, offsets))
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

    let (bytes, offsets) = read_rows(&args.rows)?;
    let column = Column::compress(&bytes, &offsets, DEFAULT_CONFIG)
        .map_err(|error| format!("compression failed: {error}"))?;
    let view = column.view();
    let frequencies = build_token_frequency_index(view.codes, view.dict.num_tokens())
        .map_err(|error| format!("frequency index: {error}"))?;

    let title = args.title.clone().unwrap_or_else(|| {
        args.rows
            .first()
            .and_then(|path| path.file_stem())
            .map_or_else(
                || "column".to_string(),
                |stem| stem.to_string_lossy().into(),
            )
    });
    println!(
        "column: {} rows, {} codes, {} dictionary tokens",
        view.num_rows(),
        view.codes.len(),
        view.dict.num_tokens()
    );

    fs::create_dir_all(&args.out).map_err(|error| format!("{}: {error}", args.out.display()))?;
    let mut gallery = BTreeMap::new();
    let mut unsound = 0usize;

    for (index, pattern) in args.patterns.iter().enumerate() {
        let options = Options {
            metric: args.metric,
            max_states: args.max_states,
            title: title.clone(),
            subtitle: None,
            measure: args.measure,
        };
        let figure = visualize(view, &frequencies, pattern, &options)
            .map_err(|error| format!("{}: {error}", String::from_utf8_lossy(pattern)))?;

        let stem = format!("{:02}-{}", index + 1, slug(pattern));
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
            |m| {
                let theirs = m.onpair_candidates.map_or_else(
                    || {
                        m.onpair_refusal
                            .clone()
                            .unwrap_or_else(|| "n/a".to_string())
                    },
                    |count| count.to_string(),
                );
                format!(
                    "{} candidates ({} exact), onpair {theirs}, sound {}",
                    m.candidates, m.exact_rows, m.sound
                )
            },
        );
        if figure.measurement.as_ref().is_some_and(|m| !m.sound) {
            unsound += 1;
        }
        let pruned = if figure.dead_probes.is_empty() {
            String::new()
        } else {
            format!(" ({} pruned as never used)", figure.dead_probes.len())
        };
        println!(
            "  {stem}: {} alignments, {} states, cut {} probes{pruned} weight {} · {measured}",
            figure.graph.stats.feasible_alignments,
            figure.states(),
            figure.cut.selected_nodes.len(),
            figure.cut.value
        );

        if let Some(svg_name) = svg_name {
            gallery.insert(
                stem,
                (String::from_utf8_lossy(pattern).into_owned(), svg_name),
            );
        }
    }

    if args.gallery {
        let mut index = String::from(
            "# Min-cut graph gallery\n\nEach row is a feasible starting alignment. Paths merge at \
             shared needle byte offsets; orange probes form the global cut, and faded nodes lie \
             downstream of it.\n",
        );
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
