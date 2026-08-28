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

use onpair::search::index::build_token_frequency_index;
use onpair::{Column, ColumnView, CompactDictionary, DEFAULT_CONFIG, Dictionary, DictionaryView, Token};
use onpair_graph_viz::{Options, ProbeWeight, predicate_text, visualize};

const USAGE: &str = "\
onpair-graph-viz — render OnPair's prefilter alignment DAG and its minimum cut

USAGE:
    onpair-graph-viz --rows FILE --pattern STR [--pattern STR ...] [OPTIONS]

INPUT:
    --rows FILE          Newline-delimited rows to train the dictionary on.
                         Repeatable; \"-\" reads standard input.
    --pattern STR        Pattern to visualize. Repeatable.
    --pattern-hex HEX    Same, given as hex bytes (e.g. 2f696e646578).

OUTPUT:
    Per pattern, DIR holds NN-slug.json, NN-slug.svg (the alignment DAG) and
    NN-slug-table.svg (every dictionary token, this cover's members marked).

OPTIONS:
    --dict D             Dictionary to draw the graph over: onpair (default) or
                         fsst. The fsst path trains an FSST symbol table over the
                         rows and transcodes it into a conformant onpair
                         dictionary: the symbols plus all 256 single-byte tokens,
                         sorted, with escapes rewritten to the byte they carry.
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

/// Which dictionary the figure is drawn over.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Dict {
    OnPair,
    Fsst,
}

struct Args {
    rows: Vec<PathBuf>,
    dict: Dict,
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
        dict: Dict::OnPair,
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
            "--dict" => {
                args.dict = match value(&flag)?.as_str() {
                    "onpair" => Dict::OnPair,
                    "fsst" => Dict::Fsst,
                    other => return Err(format!("unknown dictionary {other:?}")),
                }
            }
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

/// What a [`ColumnView`] borrows from. A view is three slices, so whatever built
/// them has to outlive it: the OnPair path owns a compressed column, the FSST
/// path owns the transcoded dictionary and stream.
enum Source {
    OnPair(Column<u32>),
    Fsst {
        dict: CompactDictionary,
        codes: Vec<Token>,
        row_offsets: Vec<u32>,
    },
}

impl Source {
    fn view(&self) -> ColumnView<'_, u32> {
        match self {
            Self::OnPair(column) => column.view(),
            Self::Fsst {
                dict,
                codes,
                row_offsets,
            } => ColumnView {
                dict: dict.as_view(),
                codes,
                row_offsets,
            },
        }
    }

    /// The escape code the figure has to plan around, which only an FSST
    /// dictionary has.
    fn escape_token(&self) -> Option<Token> {
        match self {
            Self::OnPair(_) => None,
            Self::Fsst { .. } => Some(Token::from(fsst::ESCAPE_CODE)),
        }
    }
}

/// Train an FSST symbol table over the rows and transcode it into an OnPair
/// dictionary with `onpair::fsst::transcode_fsst_to_onpair`.
///
/// The dictionary is the symbol table and nothing else, so its alphabet is
/// incomplete — most single bytes have no token — and the code stream keeps FSST's
/// escapes: a marker followed by the literal byte, both riding along as codes.
/// That is why the frequency index is built over the whole code space rather than
/// the dictionary, and why the figure is drawn with [`ESCAPE_CODE`] as its escape
/// token: the planner continues a parse it cannot tokenize through the escape.
fn compress_fsst(bytes: &[u8], offsets: &[u32]) -> Result<Source, String> {
    let lines: Vec<&[u8]> = offsets
        .windows(2)
        .map(|pair| &bytes[pair[0] as usize..pair[1] as usize])
        .collect();
    let compressor = fsst::Compressor::train(&lines);
    let per_row = compressor.compress_bulk(&lines);

    // The transcode is one byte in, one byte out — escapes included — so a row's
    // code count is its compressed byte count.
    let mut row_offsets = vec![0u32];
    for row in &per_row {
        let end = u32::try_from(row.len())
            .ok()
            .and_then(|len| row_offsets.last()?.checked_add(len))
            .ok_or_else(|| "the code stream exceeds 4 GiB of codes".to_string())?;
        row_offsets.push(end);
    }
    let (dict, remapped) = onpair::fsst::transcode_fsst_to_onpair(&compressor, &per_row.concat())
        .map_err(|error| format!("transcoding the FSST symbol table: {error}"))?;

    let escapes = remapped
        .iter()
        .filter(|&&code| code == fsst::ESCAPE_CODE)
        .count();
    println!(
        "fsst: {} symbols, {} tokens, {escapes} escapes in {} codes",
        compressor.n_symbols(),
        dict.as_view().num_tokens(),
        remapped.len()
    );

    Ok(Source::Fsst {
        dict,
        codes: remapped.iter().map(|&code| Token::from(code)).collect(),
        row_offsets,
    })
}

fn run() -> Result<(), String> {
    let Some(args) = parse_args()? else {
        print!("{USAGE}");
        return Ok(());
    };

    let (bytes, offsets) = read_rows(&args.rows)?;
    let source = match args.dict {
        Dict::OnPair => Source::OnPair(
            Column::compress(&bytes, &offsets, DEFAULT_CONFIG)
                .map_err(|error| format!("compression failed: {error}"))?,
        ),
        Dict::Fsst => compress_fsst(&bytes, &offsets)?,
    };
    let view = source.view();
    // Over the code space, not the dictionary: an FSST stream carries escape
    // markers and the literal bytes behind them as codes of their own, and a code
    // the index does not cover cannot be weighed.
    let escape_token = source.escape_token();
    let code_space = view
        .codes
        .iter()
        .map(|&code| code as usize + 1)
        .max()
        .unwrap_or(0)
        .max(view.dict.num_tokens())
        // The escape code is weighed like any other, and a stream that happens to
        // escape nothing still has to leave room for it.
        .max(escape_token.map_or(0, |escape| escape as usize + 1));
    let frequencies = build_token_frequency_index(view.codes, code_space)
        .map_err(|error| format!("frequency index: {error}"))?;

    let title = args.title.clone().unwrap_or_else(|| {
        let stem = args.rows.first().and_then(|path| path.file_stem()).map_or_else(
            || "column".to_string(),
            |stem| stem.to_string_lossy().into(),
        );
        match args.dict {
            Dict::OnPair => stem,
            Dict::Fsst => format!("{stem} · fsst"),
        }
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
            escape_token,
            // An FSST column is not decodable by OnPair, so the true matches come
            // from the rows as they were read rather than from decoding them back.
            true_rows: escape_token.map(|_| {
                (0..offsets.len() - 1)
                    .filter(|&row| {
                        let (from, to) = (offsets[row] as usize, offsets[row + 1] as usize);
                        bytes[from..to]
                            .windows(pattern.len())
                            .any(|window| window == pattern.as_slice())
                    })
                    .collect()
            }),
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

        let table_path = args.out.join(format!("{stem}-table.svg"));
        fs::write(&table_path, &figure.table_svg)
            .map_err(|error| format!("{}: {error}", table_path.display()))?;

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
        // Cut and cover are separate counts on purpose: the cut is parse steps, the
        // cover is what the scan compares against, and the mandatory whole-needle
        // tokens are in the second without ever being in the first.
        println!(
            "  {stem}: {} alignments, cut {} steps weight {} · cover {} probes ({} whole-needle) · {measured}",
            figure.graph.stats.feasible_alignments,
            figure.cut.selected_edges.len(),
            figure.cut.value,
            figure.cover.cmp_cost,
            figure.graph.contained.len()
        );

        println!(
            "    candidate if code is {}",
            predicate_text(&figure.graph, &figure.summary.cover_terms)
        );

        if let Some(svg_name) = svg_name {
            gallery.insert(
                stem,
                (String::from_utf8_lossy(pattern).into_owned(), svg_name),
            );
        }
    }

    if args.gallery {
        let mut index = String::from("# Min-cut graph gallery\n");
        for (stem, (pattern, svg)) in &gallery {
            let _ = write!(
                index,
                "\n## `{pattern}`\n\n![{pattern}]({svg})\n\n![symbol table]({stem}-table.svg)\n\n[Graph JSON]({stem}.json)\n"
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
