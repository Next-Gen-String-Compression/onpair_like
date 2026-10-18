//! Command-line front end: a plan log in, figures out.

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use onpair_graph_viz::{Options, predicate_text, read_log, visualize};

const USAGE: &str = "\
onpair-graph-viz — draw a prefilter's alignment graph and min cut from a plan log

USAGE:
    onpair-graph-viz --log DIR [FILTERS] [OPTIONS]

INPUT:
    --log DIR            Directory holding queries.csv, symbols.csv and graph.csv,
                         as onpair's examples/plan_log.rs and the fsst fork's
                         search/tools/plan_log.cpp write them.

FILTERS (every query matching all given filters is drawn):
    --uuid U             One query.
    --dataset D
    --row-group N
    --algorithm A        onpair12, onpair16 or fsst.
    --needle STR
    --needle-hex HEX     Same, given as hex bytes (e.g. 2f696e646578).

OUTPUT:
    Per query, DIR holds SLUG-ALGORITHM-UUID8.svg (the alignment DAG) and
    SLUG-ALGORITHM-UUID8-table.svg (every symbol, this cover's members marked).

OPTIONS:
    --out DIR            Where to write figures (default: out).
    --max-states N       Skip the SVG past N byte-offset states (default: 128;
                         0 disables the guard).
    --title T            Figure headline (default: dataset, row group, algorithm).
    --gallery            Also write index.md linking every figure.
    -h, --help           Print this.
";

#[derive(Default)]
struct Args {
    log: Option<PathBuf>,
    uuid: Option<String>,
    dataset: Option<String>,
    row_group: Option<usize>,
    algorithm: Option<String>,
    needle: Option<Vec<u8>>,
    out: Option<PathBuf>,
    max_states: Option<Option<usize>>,
    title: Option<String>,
    gallery: bool,
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args::default();
    let mut raw = std::env::args().skip(1);
    while let Some(flag) = raw.next() {
        let mut value = |flag: &str| raw.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--log" => args.log = Some(PathBuf::from(value(&flag)?)),
            "--uuid" => args.uuid = Some(value(&flag)?),
            "--dataset" => args.dataset = Some(value(&flag)?),
            "--row-group" => {
                args.row_group = Some(
                    value(&flag)?
                        .parse()
                        .map_err(|_| "--row-group needs a number".to_string())?,
                )
            }
            "--algorithm" => args.algorithm = Some(value(&flag)?),
            "--needle" => args.needle = Some(value(&flag)?.into_bytes()),
            "--needle-hex" => args.needle = Some(parse_hex(&value(&flag)?)?),
            "--out" => args.out = Some(PathBuf::from(value(&flag)?)),
            "--max-states" => {
                let count: usize = value(&flag)?
                    .parse()
                    .map_err(|_| "--max-states needs a number".to_string())?;
                args.max_states = Some((count > 0).then_some(count));
            }
            "--title" => args.title = Some(value(&flag)?),
            "--gallery" => args.gallery = true,
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if args.log.is_none() {
        return Err("--log is required".to_string());
    }
    Ok(Some(args))
}

fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if !cleaned.len().is_multiple_of(2) {
        return Err("--needle-hex needs an even number of digits".to_string());
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&cleaned[i..i + 2], 16)
                .map_err(|_| format!("{:?} is not hex", &cleaned[i..i + 2]))
        })
        .collect()
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
    let log = read_log(args.log.as_deref().unwrap())?;
    let out = args.out.clone().unwrap_or_else(|| PathBuf::from("out"));
    let options = Options {
        max_states: args
            .max_states
            .unwrap_or(Some(onpair_graph_viz::DEFAULT_MAX_STATES)),
        title: args.title.clone(),
        subtitle: None,
    };

    let chosen: Vec<_> = log
        .queries
        .iter()
        .filter(|q| args.uuid.as_ref().is_none_or(|u| &q.uuid == u))
        .filter(|q| args.dataset.as_ref().is_none_or(|d| &q.dataset == d))
        .filter(|q| args.row_group.is_none_or(|r| q.row_group == r))
        .filter(|q| args.algorithm.as_ref().is_none_or(|a| &q.algorithm == a))
        .filter(|q| args.needle.as_ref().is_none_or(|n| &q.needle == n))
        .collect();
    if chosen.is_empty() {
        return Err("no query in the log matches the filters".to_string());
    }

    fs::create_dir_all(&out).map_err(|error| format!("{}: {error}", out.display()))?;
    let mut gallery = String::from("# Min-cut graph gallery\n");
    for query in chosen {
        let figure = visualize(&log, query, &options)?;
        let stem = format!("{}-{}-{}", slug(&query.needle), query.algorithm, &query.uuid[..8]);

        let table_path = out.join(format!("{stem}-table.svg"));
        fs::write(&table_path, &figure.table_svg)
            .map_err(|error| format!("{}: {error}", table_path.display()))?;
        match &figure.svg {
            Some(svg) => {
                let svg_path = out.join(format!("{stem}.svg"));
                fs::write(&svg_path, svg)
                    .map_err(|error| format!("{}: {error}", svg_path.display()))?;
                let pattern = String::from_utf8_lossy(&query.needle);
                let _ = write!(
                    gallery,
                    "\n## `{pattern}` · {}\n\n![{pattern}]({stem}.svg)\n\n![symbol table]({stem}-table.svg)\n",
                    figure.summary.title
                );
            }
            None => println!(
                "  {stem}: skipped SVG, {} states exceeds --max-states",
                figure.states()
            ),
        }

        let graph = &figure.logged.graph;
        println!(
            "  {stem}: {} alignments, {} edges, cut {} edges + {} paired · cover {} probes",
            graph.stats.feasible_alignments,
            graph.edges.len(),
            figure.logged.cut.len(),
            figure.logged.paired.len(),
            figure.cover.shape.cmp_cost,
        );
        println!(
            "    candidate if code is {}",
            predicate_text(graph, &figure.summary.cover_terms, &figure.summary.cover_pairs)
        );
    }

    if args.gallery {
        let path = out.join("index.md");
        fs::write(&path, gallery).map_err(|error| format!("{}: {error}", path.display()))?;
        println!("gallery: {}", path.display());
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
