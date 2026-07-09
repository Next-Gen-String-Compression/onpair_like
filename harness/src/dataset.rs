//! The canonical dataset artifact and the single load path (DESIGN.md §4, §6).
//!
//! `bench ingest` converts an arbitrary source (Parquet/CSV/TSV) into
//! `data.arrow` (one record batch, one LargeBinary column, uncompressed,
//! 64-byte-aligned buffers) + `manifest.json` (provenance, stats, and the
//! xxh3 logical checksum that is the dataset's identity). Loading reads the
//! IPC file into aligned buffers once (setup cost, outside all measurement;
//! the zero-copy mmap variant remains a drop-in optimization of this one
//! function) and hands every candidate the identical `(bytes, offsets)` pair.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use arrow::array::{Array, LargeBinaryArray, LargeBinaryBuilder};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::ipc::writer::{FileWriter, IpcWriteOptions};
use arrow::record_batch::RecordBatch;
use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::Xxh3;

pub const DATA_FILE: &str = "data.arrow";
pub const MANIFEST_FILE: &str = "manifest.json";
pub const COLUMN_NAME: &str = "data";
const IPC_ALIGNMENT: usize = 64;

pub type Error = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, Error>;

// ------------------------------------------------------------- manifest

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRecipe {
    pub path: String,
    pub format: String,
    pub column: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetManifest {
    pub format_version: u32,
    pub id: String,
    pub source: SourceRecipe,
    pub ingested_at: String,
    pub num_rows: u64,
    pub payload_bytes: u64,
    pub nulls_removed: u64,
    pub min_len: u64,
    pub max_len: u64,
    pub mean_len: f64,
    /// Occurrences of each byte value across the whole payload; used at
    /// bless time to derive needle-rarity metadata.
    pub byte_freq: Vec<u64>,
    /// xxh3 over the logical content (per row: u64-LE length, then bytes).
    /// This is the dataset's identity: truth and results bind to it.
    pub checksum: String,
}

impl DatasetManifest {
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    }
}

fn logical_checksum<'a>(rows: impl Iterator<Item = &'a [u8]>) -> String {
    let mut h = Xxh3::new();
    for row in rows {
        h.update(&(row.len() as u64).to_le_bytes());
        h.update(row);
    }
    format!("xxh3:{:016x}", h.digest())
}

// --------------------------------------------------------------- ingest

pub struct IngestRequest {
    pub source: PathBuf,
    pub format: String, // "parquet" | "csv" | "tsv"
    pub column: String,
    pub id: String,
    pub out_dir: PathBuf,
}

/// Deterministic: same source + same options => byte-identical data.arrow
/// and identical checksum (the manifest's `ingested_at` is metadata only).
pub fn ingest(req: &IngestRequest) -> Result<DatasetManifest> {
    let mut builder = LargeBinaryBuilder::new();
    let nulls_removed = match req.format.as_str() {
        "parquet" => ingest_parquet(&req.source, &req.column, &mut builder)?,
        "csv" => ingest_delimited(&req.source, &req.column, b',', &mut builder)?,
        "tsv" => ingest_delimited(&req.source, &req.column, b'\t', &mut builder)?,
        other => return Err(format!("unknown source format {other:?}").into()),
    };
    let array = builder.finish();
    if array.is_empty() {
        return Err("ingest produced 0 rows — refusing to write an empty dataset".into());
    }

    // Stats + checksum over the logical content.
    let num_rows = array.len() as u64;
    let (mut min_len, mut max_len, mut total) = (u64::MAX, 0u64, 0u64);
    let mut byte_freq = vec![0u64; 256];
    for i in 0..array.len() {
        let row = array.value(i);
        let len = row.len() as u64;
        min_len = min_len.min(len);
        max_len = max_len.max(len);
        total += len;
        for &b in row {
            byte_freq[b as usize] += 1;
        }
    }
    let checksum = logical_checksum((0..array.len()).map(|i| array.value(i)));

    std::fs::create_dir_all(&req.out_dir)?;
    write_artifact(&req.out_dir.join(DATA_FILE), &array)?;

    let manifest = DatasetManifest {
        format_version: 1,
        id: req.id.clone(),
        source: SourceRecipe {
            path: req.source.display().to_string(),
            format: req.format.clone(),
            column: req.column.clone(),
        },
        ingested_at: humantime::format_rfc3339_seconds(std::time::SystemTime::now()).to_string(),
        num_rows,
        payload_bytes: total,
        nulls_removed,
        min_len,
        max_len,
        mean_len: total as f64 / num_rows as f64,
        byte_freq,
        checksum,
    };
    std::fs::write(
        req.out_dir.join(MANIFEST_FILE),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    Ok(manifest)
}

fn write_artifact(path: &Path, array: &LargeBinaryArray) -> Result<()> {
    let schema = Schema::new(vec![Field::new(COLUMN_NAME, DataType::LargeBinary, false)]);
    let batch = RecordBatch::try_new(schema.clone().into(), vec![std::sync::Arc::new(array.clone())])?;
    let opts = IpcWriteOptions::try_new(
        IPC_ALIGNMENT,
        false,
        arrow::ipc::MetadataVersion::V5,
    )?;
    let file = BufWriter::new(File::create(path)?);
    let mut writer = FileWriter::try_new_with_options(file, &schema, opts)?;
    writer.write(&batch)?;
    writer.finish()?;
    Ok(())
}

/// Append every non-null value of a string-ish arrow array; returns nulls seen.
fn append_string_array(array: &dyn Array, out: &mut LargeBinaryBuilder) -> Result<u64> {
    use arrow::array::{
        BinaryArray, BinaryViewArray, LargeStringArray, StringArray, StringViewArray,
    };
    let mut nulls = 0u64;
    macro_rules! copy {
        ($arr:expr) => {
            for i in 0..$arr.len() {
                if $arr.is_null(i) {
                    nulls += 1;
                } else {
                    out.append_value($arr.value(i));
                }
            }
        };
    }
    let any = array.as_any();
    if let Some(a) = any.downcast_ref::<LargeBinaryArray>() {
        copy!(a);
    } else if let Some(a) = any.downcast_ref::<BinaryArray>() {
        copy!(a);
    } else if let Some(a) = any.downcast_ref::<StringArray>() {
        copy!(a);
    } else if let Some(a) = any.downcast_ref::<LargeStringArray>() {
        copy!(a);
    } else if let Some(a) = any.downcast_ref::<StringViewArray>() {
        copy!(a);
    } else if let Some(a) = any.downcast_ref::<BinaryViewArray>() {
        copy!(a);
    } else {
        return Err(format!(
            "column has unsupported type {:?} (expected a string/binary type)",
            array.data_type()
        )
        .into());
    }
    Ok(nulls)
}

fn ingest_parquet(source: &Path, column: &str, out: &mut LargeBinaryBuilder) -> Result<u64> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::arrow::ProjectionMask;

    let file = File::open(source).map_err(|e| format!("opening {}: {e}", source.display()))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let idx = builder
        .schema()
        .fields()
        .iter()
        .position(|f| f.name() == column)
        .ok_or_else(|| {
            format!(
                "column {column:?} not found; available: {:?}",
                builder.schema().fields().iter().map(|f| f.name().clone()).collect::<Vec<_>>()
            )
        })?;
    let mask = ProjectionMask::roots(builder.parquet_schema(), [idx]);
    let reader = builder.with_projection(mask).build()?;

    let mut nulls = 0u64;
    for batch in reader {
        let batch = batch?;
        nulls += append_string_array(batch.column(0).as_ref(), out)?;
    }
    Ok(nulls)
}

fn ingest_delimited(
    source: &Path,
    column: &str,
    delimiter: u8,
    out: &mut LargeBinaryBuilder,
) -> Result<u64> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .from_path(source)
        .map_err(|e| format!("opening {}: {e}", source.display()))?;
    let headers = reader.byte_headers()?.clone();
    let idx = headers
        .iter()
        .position(|h| h == column.as_bytes())
        .or_else(|| column.parse::<usize>().ok().filter(|&i| i < headers.len()))
        .ok_or_else(|| {
            format!(
                "column {column:?} not found in header (and not a valid index); header: {:?}",
                headers.iter().map(|h| String::from_utf8_lossy(h).into_owned()).collect::<Vec<_>>()
            )
        })?;
    let mut record = csv::ByteRecord::new();
    while reader.read_byte_record(&mut record)? {
        // Delimited files have no null representation: every field is a
        // byte string (possibly empty). Nulls exist only for parquet.
        out.append_value(record.get(idx).ok_or("short record")?);
    }
    Ok(0)
}

// ----------------------------------------------------------------- load

/// The one prepared in-memory form every candidate and the oracle consume.
pub struct PreparedDataset {
    array: LargeBinaryArray,
    pub manifest: DatasetManifest,
    pub dir: PathBuf,
}

impl PreparedDataset {
    /// Load + validate a canonical artifact. `verify_checksum` recomputes
    /// the logical checksum against the manifest (cheap relative to a run;
    /// skippable for interactive iteration).
    pub fn load(dir: &Path, verify_checksum: bool) -> Result<Self> {
        let manifest = DatasetManifest::load(dir)?;
        let file = File::open(dir.join(DATA_FILE))
            .map_err(|e| format!("opening {}: {e}", dir.join(DATA_FILE).display()))?;
        let mut reader = arrow::ipc::reader::FileReader::try_new(file, None)?;
        let batch = reader
            .next()
            .ok_or("canonical artifact contains no record batch")??;
        if reader.next().is_some() {
            return Err("canonical artifact must contain exactly one record batch".into());
        }
        if batch.num_columns() != 1 {
            return Err("canonical artifact must contain exactly one column".into());
        }
        let array = batch
            .column(0)
            .as_any()
            .downcast_ref::<LargeBinaryArray>()
            .ok_or_else(|| {
                format!(
                    "canonical column must be LargeBinary, found {:?}",
                    batch.column(0).data_type()
                )
            })?
            .clone();

        // Structural validation: the ABI view depends on these invariants.
        let offsets = array.value_offsets();
        if offsets.first() != Some(&0) {
            return Err("offsets must start at 0".into());
        }
        if offsets.windows(2).any(|w| w[1] < w[0]) {
            return Err("offsets must be non-decreasing".into());
        }
        if *offsets.last().unwrap() as usize > array.values().len() {
            return Err("last offset exceeds payload length".into());
        }
        if array.len() as u64 != manifest.num_rows {
            return Err(format!(
                "row count mismatch: artifact has {}, manifest says {}",
                array.len(),
                manifest.num_rows
            )
            .into());
        }

        let ds = Self {
            array,
            manifest,
            dir: dir.to_path_buf(),
        };
        if verify_checksum {
            let actual = logical_checksum(ds.rows());
            if actual != ds.manifest.checksum {
                return Err(format!(
                    "dataset checksum mismatch: artifact hashes to {actual}, manifest says {} — \
                     the artifact was modified or corrupted; re-run `bench ingest`",
                    ds.manifest.checksum
                )
                .into());
            }
        }
        Ok(ds)
    }

    pub fn num_rows(&self) -> u64 {
        self.array.len() as u64
    }

    /// Offsets as u64 (validated non-negative, non-decreasing at load).
    pub fn offsets_u64(&self) -> &[u64] {
        let off: &[i64] = self.array.value_offsets();
        // Sound: same layout, and load() validated all values >= 0.
        unsafe { std::slice::from_raw_parts(off.as_ptr() as *const u64, off.len()) }
    }

    pub fn payload(&self) -> &[u8] {
        let end = *self.offsets_u64().last().unwrap() as usize;
        &self.array.values()[..end]
    }

    pub fn row(&self, i: u64) -> &[u8] {
        self.array.value(i as usize)
    }

    pub fn rows(&self) -> impl Iterator<Item = &[u8]> {
        (0..self.array.len()).map(|i| self.array.value(i))
    }

    /// The uncompressed baseline size: payload + 8·(num_rows+1). Ratio is
    /// derived at reporting time as raw_bytes / footprint (DESIGN.md §9).
    pub fn raw_bytes(&self) -> u64 {
        self.manifest.payload_bytes + 8 * (self.num_rows() + 1)
    }
}
