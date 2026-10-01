//! Reads the three CSVs the plan loggers write (`onpair`'s
//! `examples/plan_log.rs`, the fsst fork's `search/tools/plan_log.cpp`).

use std::collections::HashMap;
use std::path::Path;

use crate::graph::Token;

/// One row of `queries.csv`.
#[derive(Clone, Debug)]
pub struct Query {
    /// Hash of dataset, row group, needle and algorithm.
    pub uuid: String,
    /// Column the row group was cut from.
    pub dataset: String,
    /// Index of the row group in the column.
    pub row_group: usize,
    /// The pattern.
    pub needle: Vec<u8>,
    /// `onpair12`, `onpair16` or `fsst`.
    pub algorithm: String,
    /// Rows in the row group.
    pub rows: u64,
    /// Share of rows containing the needle.
    pub needle_selectivity: f64,
    /// Covered codes over rows: what the planner priced the cover at.
    pub cut_selectivity_estimated: f64,
    /// Share of rows holding a covered code.
    pub cut_selectivity_measured: f64,
}

/// What a scan compares a code against to catch one step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Label {
    /// One code.
    Code(Token),
    /// An explicit set of codes.
    CodeSet(Vec<Token>),
    /// An inclusive code range.
    CodeRange(Token, Token),
    /// The escape code followed by this literal byte.
    EscapeCode(u8),
    /// A set too large to enumerate; no cut selects it.
    None,
}

/// Whether the planner's cut takes an edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cut {
    /// Not taken.
    No,
    /// Taken on its own.
    Yes,
    /// Taken together with each of these codes before it (FSST pairs).
    Paired(Vec<Token>),
}

/// One row of `graph.csv`.
#[derive(Clone, Debug)]
pub struct LogEdge {
    /// Needle offset the step leaves.
    pub from: usize,
    /// Needle offset it reaches.
    pub to: usize,
    /// The probe.
    pub label: Label,
    /// Whether the cut takes it.
    pub cut: Cut,
}

/// One row group's symbol table, indexed by token id.
#[derive(Clone, Debug, Default)]
pub struct Symbols {
    /// Token bytes; `None` where no row names the id, empty for the escape code.
    pub content: Vec<Option<Vec<u8>>>,
    /// Occurrences of each id in the row group's code stream.
    pub frequency: Vec<u64>,
}

impl Symbols {
    /// The id whose row has empty content, which only FSST's escape code has.
    pub fn escape(&self) -> Option<Token> {
        self.content
            .iter()
            .position(|c| c.as_ref().is_some_and(Vec::is_empty))
            .map(|id| id as Token)
    }
}

/// A whole log directory.
#[derive(Clone, Debug, Default)]
pub struct Log {
    /// Every query, in file order.
    pub queries: Vec<Query>,
    /// Symbol tables by (dataset, row group, algorithm).
    pub symbols: HashMap<(String, usize, String), Symbols>,
    /// Edges by query uuid.
    pub graph: HashMap<String, Vec<LogEdge>>,
}

impl Log {
    /// The symbol table `query` was planned over.
    pub fn symbols_of(&self, query: &Query) -> Option<&Symbols> {
        self.symbols
            .get(&(query.dataset.clone(), query.row_group, query.algorithm.clone()))
    }
}

fn base64(text: &str) -> Result<Vec<u8>, String> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Ok(u32::from(c - b'A')),
        b'a'..=b'z' => Ok(u32::from(c - b'a') + 26),
        b'0'..=b'9' => Ok(u32::from(c - b'0') + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(format!("{text:?} is not base64")),
    };
    let body = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in body.as_bytes() {
        acc = (acc << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

fn ids(text: &str) -> Result<Vec<Token>, String> {
    let inner = text
        .strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .ok_or_else(|| format!("{text:?} is not a list"))?;
    inner
        .split(',')
        .map(|id| id.trim().parse().map_err(|_| format!("{id:?} is not a code")))
        .collect()
}

fn parse<T: std::str::FromStr>(text: &str, what: &str) -> Result<T, String> {
    text.parse().map_err(|_| format!("{what} {text:?} does not parse"))
}

/// The rows of `path` as maps from header name to field.
fn rows(path: &Path) -> Result<Vec<HashMap<String, String>>, String> {
    let mut reader =
        csv::Reader::from_path(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let header: Vec<String> = reader
        .headers()
        .map_err(|e| format!("{}: {e}", path.display()))?
        .iter()
        .map(str::to_string)
        .collect();
    reader
        .records()
        .map(|record| {
            let record = record.map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(header.iter().cloned().zip(record.iter().map(str::to_string)).collect())
        })
        .collect()
}

/// Read `queries.csv`, `symbols.csv` and `graph.csv` from `dir`.
///
/// # Errors
/// A missing file or a field that does not parse.
pub fn read_log(dir: &Path) -> Result<Log, String> {
    let mut log = Log::default();
    for r in rows(&dir.join("queries.csv"))? {
        log.queries.push(Query {
            uuid: r["uuid"].clone(),
            dataset: r["dataset"].clone(),
            row_group: parse(&r["row_group"], "row_group")?,
            needle: base64(&r["needle"])?,
            algorithm: r["algorithm"].clone(),
            rows: parse(&r["rows"], "rows")?,
            needle_selectivity: parse(&r["needle_selectivity"], "needle_selectivity")?,
            cut_selectivity_estimated: parse(
                &r["cut_selectivity_estimated"],
                "cut_selectivity_estimated",
            )?,
            cut_selectivity_measured: parse(
                &r["cut_selectivity_measured"],
                "cut_selectivity_measured",
            )?,
        });
    }
    for r in rows(&dir.join("symbols.csv"))? {
        let key = (
            r["dataset"].clone(),
            parse(&r["row_group"], "row_group")?,
            r["algorithm"].clone(),
        );
        let id: usize = parse(&r["token_id"], "token_id")?;
        let table = log.symbols.entry(key).or_default();
        if table.content.len() <= id {
            table.content.resize(id + 1, None);
            table.frequency.resize(id + 1, 0);
        }
        table.content[id] = Some(base64(&r["content"])?);
        table.frequency[id] = parse(&r["frequency"], "frequency")?;
    }
    for r in rows(&dir.join("graph.csv"))? {
        let label = &r["label"];
        let label = match r["label_type"].as_str() {
            "code" => Label::Code(parse(label, "code")?),
            "code-set" => Label::CodeSet(ids(label)?),
            "code-range" => match ids(label)?.as_slice() {
                &[lo, hi] => Label::CodeRange(lo, hi),
                _ => return Err(format!("code-range {label:?} is not two codes")),
            },
            "escape-code" => Label::EscapeCode(parse(label, "escape byte")?),
            "none" => Label::None,
            other => return Err(format!("unknown label_type {other:?}")),
        };
        let cut = match r["cut"].as_str() {
            "no" => Cut::No,
            "yes" => Cut::Yes,
            other => match other.strip_prefix("paired:") {
                Some(codes) => Cut::Paired(
                    codes
                        .split(';')
                        .map(|c| parse(c, "paired code"))
                        .collect::<Result<_, _>>()?,
                ),
                None => return Err(format!("unknown cut {other:?}")),
            },
        };
        log.graph.entry(r["uuid"].clone()).or_default().push(LogEdge {
            from: parse(&r["from"], "from")?,
            to: parse(&r["to"], "to")?,
            label,
            cut,
        });
    }
    Ok(log)
}
