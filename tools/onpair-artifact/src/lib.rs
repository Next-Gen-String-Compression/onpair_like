//! The exact OnPair state needed to reconstruct a benchmark min-cut graph.
//!
//! The benchmark intentionally does not serialize the code stream or row
//! offsets: graph construction needs the ordered compact dictionary and token
//! frequency index only. Keeping this codec beside the one shared OnPair pin
//! makes the producer and consumer move together.

pub use onpair_core::*;

use onpair_core::search::index::{
    OwnedTokenFrequencyIndexStorage, TokenFrequencyIndex, TokenFrequencyIndexStorage,
};
use onpair_core::{
    CompactDictionary as ArtifactDictionary, Dictionary as ArtifactDictionaryTrait,
    DictionaryView as ArtifactDictionaryView, OwnedDictionaryStorage as ArtifactDictionaryStorage,
    Token as ArtifactToken,
};
use xxhash_rust::xxh3::Xxh3;

/// Candidate ABI format tag for this sidecar.
pub const FORMAT: &str = "onpair-mincut-sidecar-v1";

/// NUL-terminated form of [`FORMAT`] for candidate ABI descriptors.
pub const FORMAT_CSTR: &core::ffi::CStr = c"onpair-mincut-sidecar-v1";

/// Stable prefix used in query rows and graph bundles.
pub const FINGERPRINT_PREFIX: &str = "onpair-mincut-v1:";

const MAGIC: &[u8; 8] = b"LBOPMC01";
const HEADER_LEN: usize = 64;

/// A validated, owned min-cut sidecar.
#[derive(Debug)]
pub struct Artifact {
    /// Actual code width required by the serialized dictionary. The configured
    /// bit budget remains in the benchmark result key.
    pub dictionary_bits: u8,
    /// Exact compact dictionary bytes, including decoder read padding.
    pub dictionary: ArtifactDictionary,
    /// Exact cumulative token-frequency index.
    pub frequencies: TokenFrequencyIndex,
    /// Stable identity of the two structures above.
    pub fingerprint: u64,
    /// Code positions represented by `frequencies`.
    pub indexed_codes: u64,
}

/// Versioned identity of the state that determines a term-frequency min-cut.
pub fn mincut_fingerprint<S: TokenFrequencyIndexStorage>(
    dictionary: impl ArtifactDictionaryView,
    frequencies: &TokenFrequencyIndex<S>,
) -> u64 {
    let mut hash = Xxh3::new();
    hash.update(b"onpair-mincut-dictionary-v1\0");
    hash.update(&(dictionary.num_tokens() as u64).to_le_bytes());
    for id in 0..dictionary.num_tokens() {
        let token = dictionary.token(id as ArtifactToken);
        hash.update(&(token.len() as u64).to_le_bytes());
        hash.update(token);
        hash.update(&frequencies.frequency(id as Token).to_le_bytes());
    }
    // u64::MAX is the benchmark ABI's unset sentinel. Keep every serialized
    // artifact representable in query facts without depending on lb-abi here.
    hash.digest().min(u64::MAX - 1)
}

/// Format a fingerprint for JSON result rows and graph bundles.
pub fn fingerprint_text(value: u64) -> String {
    format!("{FINGERPRINT_PREFIX}{value:016x}")
}

/// Serialize an exact compact dictionary and frequency index.
pub fn encode_sidecar<S: TokenFrequencyIndexStorage>(
    dictionary: &ArtifactDictionary,
    frequencies: &TokenFrequencyIndex<S>,
) -> Vec<u8> {
    let dictionary_bits = dictionary.code_bits();
    let dictionary_bytes = dictionary.bytes();
    let offsets = dictionary.offsets();
    let cumulative = frequencies.storage().cumulative();
    let indexed_codes = cumulative.last().copied().unwrap_or(0) as u64;
    let identity = mincut_fingerprint(dictionary.as_view(), frequencies);
    let mut output = Vec::with_capacity(
        HEADER_LEN + dictionary_bytes.len() + 4 * (offsets.len() + cumulative.len()),
    );
    output.extend_from_slice(MAGIC);
    output.push(dictionary_bits);
    output.extend_from_slice(&[0; 7]);
    for value in [
        identity,
        dictionary.num_tokens() as u64,
        indexed_codes,
        dictionary_bytes.len() as u64,
        offsets.len() as u64,
        cumulative.len() as u64,
    ] {
        output.extend_from_slice(&value.to_le_bytes());
    }
    output.extend_from_slice(dictionary_bytes);
    for &value in offsets {
        output.extend_from_slice(&value.to_le_bytes());
    }
    for &value in cumulative {
        output.extend_from_slice(&value.to_le_bytes());
    }
    output
}

/// Parse and fully validate an exact sidecar.
pub fn decode_sidecar(input: &[u8]) -> Result<Artifact, String> {
    if input.len() < HEADER_LEN || &input[..8] != MAGIC {
        return Err("not an onpair-mincut-sidecar-v1 artifact".into());
    }
    let dictionary_bits = input[8];
    if !(1..=16).contains(&dictionary_bits) {
        return Err(format!("invalid dictionary bit budget {dictionary_bits}"));
    }
    if input[9..16].iter().any(|&byte| byte != 0) {
        return Err("non-zero reserved sidecar header bytes".into());
    }
    let mut cursor = 16;
    let mut next_u64 = || {
        let value = u64::from_le_bytes(input[cursor..cursor + 8].try_into().unwrap());
        cursor += 8;
        value
    };
    let recorded_fingerprint = next_u64();
    let num_tokens = usize::try_from(next_u64()).map_err(|_| "token count is too large")?;
    let indexed_codes = next_u64();
    let dictionary_bytes_len =
        usize::try_from(next_u64()).map_err(|_| "dictionary byte length is too large")?;
    let offset_count = usize::try_from(next_u64()).map_err(|_| "offset count is too large")?;
    let frequency_count =
        usize::try_from(next_u64()).map_err(|_| "frequency count is too large")?;
    if offset_count != num_tokens.saturating_add(1)
        || frequency_count != num_tokens.saturating_add(1)
    {
        return Err("sidecar token, offset, and frequency counts disagree".into());
    }
    let expected_len = HEADER_LEN
        .checked_add(dictionary_bytes_len)
        .and_then(|len| len.checked_add(offset_count.checked_mul(4)?))
        .and_then(|len| len.checked_add(frequency_count.checked_mul(4)?))
        .ok_or("sidecar length overflow")?;
    if input.len() != expected_len {
        return Err(format!(
            "sidecar has {} bytes, header describes {expected_len}",
            input.len()
        ));
    }
    let dictionary_end = HEADER_LEN + dictionary_bytes_len;
    let dictionary_bytes = input[HEADER_LEN..dictionary_end].to_vec();
    let offsets_end = dictionary_end + offset_count * 4;
    let offsets = read_u32s(&input[dictionary_end..offsets_end]);
    let cumulative = read_u32s(&input[offsets_end..]);
    let dictionary =
        ArtifactDictionary::validate(ArtifactDictionaryStorage::new(dictionary_bytes, offsets))
            .map_err(|error| format!("invalid sidecar dictionary: {error}"))?;
    let indexed_codes_usize =
        usize::try_from(indexed_codes).map_err(|_| "indexed code count is too large")?;
    let frequencies = TokenFrequencyIndex::validate_safety(
        OwnedTokenFrequencyIndexStorage::new(cumulative),
        num_tokens,
        indexed_codes_usize,
    )
    .map_err(|error| format!("invalid sidecar frequency index: {error}"))?;
    let actual_fingerprint = mincut_fingerprint(dictionary.as_view(), &frequencies);
    if actual_fingerprint != recorded_fingerprint {
        return Err(format!(
            "sidecar fingerprint mismatch: recorded {}, computed {}",
            fingerprint_text(recorded_fingerprint),
            fingerprint_text(actual_fingerprint)
        ));
    }
    Ok(Artifact {
        dictionary_bits,
        dictionary,
        frequencies,
        fingerprint: actual_fingerprint,
        indexed_codes,
    })
}

fn read_u32s(input: &[u8]) -> Vec<u32> {
    input
        .chunks_exact(4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use onpair_core::search::index::build_token_frequency_index;
    use onpair_core::{Column, Config, MaxDictBits};

    #[test]
    fn round_trip_preserves_dictionary_and_frequency_identity() {
        let rows = b"alphabetagammaalpha";
        let offsets = [0u32, 5, 9, 14, 19];
        let column = Column::compress(
            rows,
            &offsets,
            Config {
                max_dict_bits: MaxDictBits::new(9).unwrap(),
                seed: Some(42),
                ..Config::default()
            },
        )
        .unwrap();
        let frequencies =
            build_token_frequency_index(&column.codes, column.dict.num_tokens()).unwrap();
        let encoded = encode_sidecar(&column.dict, &frequencies);
        let decoded = decode_sidecar(&encoded).unwrap();

        assert_eq!(decoded.dictionary_bits, column.dict.code_bits());
        assert_eq!(decoded.indexed_codes, column.codes.len() as u64);
        assert_eq!(decoded.dictionary.bytes(), column.dict.bytes());
        assert_eq!(decoded.dictionary.offsets(), column.dict.offsets());
        assert_eq!(
            decoded.frequencies.storage().cumulative(),
            frequencies.storage().cumulative()
        );
        assert_eq!(
            decoded.fingerprint,
            mincut_fingerprint(column.dict.as_view(), &frequencies)
        );
    }

    #[test]
    fn corruption_is_detected() {
        let rows = b"alphaalpha";
        let offsets = [0u32, 5, 10];
        let column = Column::compress(rows, &offsets, Config::default()).unwrap();
        let frequencies =
            build_token_frequency_index(&column.codes, column.dict.num_tokens()).unwrap();
        let mut encoded = encode_sidecar(&column.dict, &frequencies);
        *encoded.last_mut().unwrap() ^= 1;
        assert!(decode_sidecar(&encoded).is_err());
    }
}
