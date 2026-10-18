// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! Regenerates the scan-benchmark corpora under `bench/data`.
//!
//!   `cargo run --release --example bench_corpus [db...]`
//!
//! Pulls one string column at a time out of the DuckDB files in `$HOME` (the
//! `duckdb` CLI has to be on `PATH`), then re-encodes each corpus as two
//! OnPair streams. Naming databases (`ch`, `imdb`, `stackoverflow`) restricts
//! the run to their columns; the default is every source. Layout is
//! `<database>/<table>/<column>_<size>[.<encoding>].csv`, one row per line:
//! a raw file holds the value, an encoded file holds that row's codes as
//! comma-separated decimals, so the row layer survives as the line structure.
//! See `data.md` for what the columns are and which of them are de-duplicated.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use onpair::{Config, DEFAULT_CONFIG, MaxDictBits, compress};

/// One column to extract. `distinct` de-duplicates the prefix *after* the
/// limit, for columns where duplicates dominate.
struct Source {
    db: &'static str,
    table: &'static str,
    column: &'static str,
    distinct: bool,
}

#[rustfmt::skip]
const SOURCES: &[Source] = &[
    // Request URLs, ASCII, long shared prefixes.
    Source { db: "ch", table: "hits", column: "URL", distinct: true },
    // Page titles, mostly Cyrillic, so multi-byte UTF-8 throughout.
    Source { db: "ch", table: "hits", column: "Title", distinct: true },
    // Biographies, quotes and trivia: long free text.
    Source { db: "imdb", table: "person_info", column: "info", distinct: false },
    // Movie titles: short, latin with accents.
    Source { db: "imdb", table: "title", column: "title", distinct: false },
    // "Surname, Firstname".
    Source { db: "imdb", table: "name", column: "name", distinct: false },
    // Character names: same shape, but nearly all unique.
    Source { db: "imdb", table: "char_name", column: "name", distinct: false },
    // Role notes, "(voice)", "(as Too Short)".
    Source { db: "imdb", table: "cast_info", column: "note", distinct: true },
    // Release notes, "(2006) (USA) (TV)".
    Source { db: "imdb", table: "movie_companies", column: "note", distinct: true },
    // Ratings and vote counts: digits and dots only.
    Source { db: "imdb", table: "movie_info_idx", column: "info", distinct: true },
    // User profile text: long, with markdown and HTML markup.
    Source { db: "stackoverflow", table: "Users", column: "AboutMe", distinct: false },
    // "Berlin, Germany", entered by hand, so short and heavily repeated.
    Source { db: "stackoverflow", table: "Users", column: "Location", distinct: true },
    // Screen names: short, near-unique, mixed case and digits.
    Source { db: "stackoverflow", table: "Users", column: "DisplayName", distinct: false },
    // Home pages: free-form URLs, 90% of them missing.
    Source { db: "stackoverflow", table: "Users", column: "WebsiteUrl", distinct: false },
];

/// Prefix lengths, and the suffix each one gets in the file name.
const SIZES: &[(&str, usize)] = &[("128k", 128 << 10), ("1m", 1024 << 10)];

/// OnPair dictionary budgets, in bits.
const ONPAIR_BITS: &[u8] = &[12, 16];

impl Source {
    fn raw_path(&self, data: &Path, size: &str) -> PathBuf {
        data.join(self.db)
            .join(self.table)
            .join(format!("{}_{size}.csv", self.column))
    }
}

fn main() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/data");
    let dbs: Vec<String> = std::env::args().skip(1).collect();
    let sources: Vec<&Source> = SOURCES
        .iter()
        .filter(|s| dbs.is_empty() || dbs.iter().any(|db| db == s.db))
        .collect();
    assert!(!sources.is_empty(), "no source in {dbs:?}");
    extract(&data, &sources);
    println!(
        "\n{:<38} {:>9} {:>10} {:>10} {:>6}",
        "file", "rows", "raw MB", "codes", "ratio"
    );
    for source in &sources {
        for (size, _) in SIZES {
            encode(&source.raw_path(&data, size), &data);
        }
    }
}

/// Write every raw corpus with one `duckdb` invocation. NULL and `''` are
/// dropped, and a newline inside a value becomes a space: either way the value
/// would not survive one row per line. Only `Users.AboutMe` is much affected,
/// where 78% of the values run over several lines.
fn extract(data: &Path, sources: &[&Source]) {
    let mut sql = String::new();
    let mut attached: Vec<&str> = Vec::new();
    for source in sources {
        if !attached.contains(&source.db) {
            attached.push(source.db);
            let db = source.db;
            writeln!(sql, "ATTACH '~/{db}.duckdb' AS {db} (READ_ONLY);").unwrap();
        }
    }
    sql.push_str(
        "CREATE OR REPLACE MACRO flatten(s) AS \
         replace(replace(s, chr(10), ' '), chr(13), ' ');\n",
    );
    for source in sources {
        let (db, table, column) = (source.db, source.table, source.column);
        writeln!(
            sql,
            "CREATE OR REPLACE TEMP VIEW src AS SELECT flatten(\"{column}\") AS v \
             FROM {db}.{table} WHERE \"{column}\" IS NOT NULL AND \"{column}\" <> '';"
        )
        .unwrap();
        for (size, rows) in SIZES {
            let path = source.raw_path(data, size);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let prefix = format!("SELECT v FROM src LIMIT {rows}");
            // First-appearance order, not `DISTINCT`: a hash-aggregate would
            // hand back a different row order on every run, and with it a
            // different trained dictionary.
            let select = if source.distinct {
                format!(
                    "SELECT v FROM (SELECT v, row_number() OVER () AS i FROM ({prefix})) \
                     GROUP BY v ORDER BY min(i)"
                )
            } else {
                prefix
            };
            writeln!(
                sql,
                "COPY ({select}) TO '{}' (FORMAT csv, HEADER false, QUOTE '', ESCAPE '');",
                path.display()
            )
            .unwrap();
        }
    }
    let mut duckdb = std::process::Command::new("duckdb")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("duckdb CLI not on PATH");
    duckdb
        .stdin
        .take()
        .unwrap()
        .write_all(sql.as_bytes())
        .unwrap();
    assert!(duckdb.wait().unwrap().success(), "duckdb extraction failed");
}

/// Re-encode one raw corpus into its code streams.
fn encode(raw: &Path, data: &Path) {
    let text = std::fs::read(raw).unwrap();
    let lines: Vec<&[u8]> = text
        .strip_suffix(b"\n")
        .unwrap_or(&text)
        .split(|&byte| byte == b'\n')
        .collect();
    let mut bytes = Vec::with_capacity(text.len());
    let mut row_offsets = Vec::with_capacity(lines.len() + 1);
    row_offsets.push(0u32);
    for line in &lines {
        bytes.extend_from_slice(line);
        row_offsets.push(bytes.len() as u32);
    }

    for &bits in ONPAIR_BITS {
        let config = Config {
            max_dict_bits: MaxDictBits::new(bits).unwrap(),
            ..DEFAULT_CONFIG
        };
        let column = compress(&bytes, &row_offsets, config).unwrap();
        let name = format!("onpair{bits}");
        write_codes(
            raw,
            &name,
            &column.codes,
            &column.row_offsets,
            bytes.len(),
            data,
        );
    }
}

/// One line of comma-separated codes per row, next to the raw file as
/// `<stem>.<encoding>.csv`.
fn write_codes<T: Copy + Into<u32>>(
    raw: &Path,
    encoding: &str,
    codes: &[T],
    row_offsets: &[u32],
    raw_bytes: usize,
    data: &Path,
) {
    let path = raw.with_extension(format!("{encoding}.csv"));
    let mut out = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let mut line = String::new();
    for window in row_offsets.windows(2) {
        line.clear();
        let row = &codes[window[0] as usize..window[1] as usize];
        for (i, &code) in row.iter().enumerate() {
            if i > 0 {
                line.push(',');
            }
            write!(line, "{}", code.into()).unwrap();
        }
        line.push('\n');
        out.write_all(line.as_bytes()).unwrap();
    }
    let stream_bytes = size_of_val(codes);
    println!(
        "{:<38} {:>9} {:>10.1} {:>10} {:>5.2}x",
        path.strip_prefix(data).unwrap().display(),
        row_offsets.len() - 1,
        raw_bytes as f64 / (1 << 20) as f64,
        codes.len(),
        raw_bytes as f64 / stream_bytes as f64,
    );
}
