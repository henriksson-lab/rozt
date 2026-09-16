//! Stage-0 decoder measurement harness.
//!
//! This intentionally uses a deterministic synthetic payload only to prove identical codec
//! measurement mechanics on native and wasm. It is not the S2 corpus: a pinned microscopy chunk
//! corpus and a `numcodecs.js` runner are required before throughput guides architecture.

use std::{
    hint::black_box,
    io::{Cursor, Read},
};
use zarrs::{
    array::codec::{blosc_compress_bytes, blosc_decompress_bytes, blosc_validate},
    metadata_ext::codec::blosc::{BloscCompressionLevel, BloscCompressor, BloscShuffleMode},
};

#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

const PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
const WARMUP_ITERATIONS: usize = 3;
const MEASURED_ITERATIONS: usize = 20;

/// Runs the identical codec/checksum loop for the native CLI and wasm export.
pub fn benchmark_json() -> Result<String, Box<dyn std::error::Error>> {
    let payload = deterministic_payload(PAYLOAD_BYTES);
    let expected_checksum = checksum(&payload);
    let zstd = zstd::stream::encode_all(Cursor::new(&payload), 3)?;
    let lz4 = lz4_flex::compress_prepend_size(&payload);
    let blosc = blosc_encode(&payload)?;
    let zstd_result = measure("zstd", &zstd, expected_checksum, |encoded| {
        zstd::stream::decode_all(Cursor::new(encoded)).map_err(Into::into)
    })?;
    let lz4_result = measure("lz4", &lz4, expected_checksum, |encoded| {
        lz4_flex::decompress_size_prepended(encoded).map_err(Into::into)
    })?;
    let blosc_result = measure("blosc-lz4-shuffle", &blosc, expected_checksum, blosc_decode)?;
    Ok(format!(
        "{{\"kind\":\"synthetic-harness\",\"payloadBytes\":{PAYLOAD_BYTES},\"checksum\":\"{expected_checksum:016x}\",\"warmupIterations\":{WARMUP_ITERATIONS},\"measuredIterations\":{MEASURED_ITERATIONS},\"results\":[{zstd_result},{lz4_result},{blosc_result}]}}"
    ))
}

/// Measures separately encoded chunks, matching the decoder work unit used by Zarr stores.
///
/// The caller owns filesystem traversal so this remains usable in wasm with in-memory chunks.
pub fn benchmark_chunks_json(chunks: &[Vec<u8>]) -> Result<String, Box<dyn std::error::Error>> {
    if chunks.is_empty() || chunks.iter().any(Vec::is_empty) {
        return Err("chunk benchmark requires one or more non-empty chunks".into());
    }
    let expected_checksums: Vec<_> = chunks.iter().map(|chunk| checksum(chunk)).collect();
    let zstd: Vec<Vec<u8>> = chunks
        .iter()
        .map(|chunk| zstd::stream::encode_all(Cursor::new(chunk), 3))
        .collect::<Result<_, _>>()?;
    let lz4: Vec<Vec<u8>> = chunks
        .iter()
        .map(|chunk| lz4_flex::compress_prepend_size(chunk))
        .collect();
    let blosc: Vec<Vec<u8>> = chunks
        .iter()
        .map(|chunk| blosc_encode(chunk))
        .collect::<Result<_, _>>()?;
    let zstd_result = measure_chunks("zstd", &zstd, &expected_checksums, |encoded| {
        zstd::stream::decode_all(Cursor::new(encoded)).map_err(Into::into)
    })?;
    let lz4_result = measure_chunks("lz4", &lz4, &expected_checksums, |encoded| {
        lz4_flex::decompress_size_prepended(encoded).map_err(Into::into)
    })?;
    let blosc_result = measure_chunks(
        "blosc-lz4-shuffle",
        &blosc,
        &expected_checksums,
        blosc_decode,
    )?;
    let payload_bytes: usize = chunks.iter().map(Vec::len).sum();
    Ok(format!(
        "{{\"kind\":\"chunk-corpus\",\"chunkCount\":{},\"payloadBytes\":{payload_bytes},\"warmupIterations\":{WARMUP_ITERATIONS},\"measuredIterations\":{MEASURED_ITERATIONS},\"results\":[{zstd_result},{lz4_result},{blosc_result}]}}",
        chunks.len()
    ))
}

fn blosc_encode(bytes: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    blosc_compress_bytes(
        bytes,
        BloscCompressionLevel::try_from(5).expect("Blosc compression level five is valid"),
        BloscShuffleMode::Shuffle,
        2,
        BloscCompressor::LZ4,
        0,
        1,
    )
    .map_err(Into::into)
}

fn blosc_decode(encoded: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let decoded_size = blosc_validate(encoded).ok_or("invalid Blosc buffer")?;
    blosc_decompress_bytes(encoded, decoded_size, 1).map_err(Into::into)
}

/// Decode one Zstd-compressed Zarr chunk. The caller validates the Zarr codec chain and expected
/// uncompressed length; this primitive deliberately has no store or browser policy knowledge.
pub fn decode_zstd_chunk(encoded: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    decode_zstd_chunk_bounded(encoded, usize::MAX)
}

/// Decode Zstd while refusing to grow the output beyond the caller's decompression budget.
/// Streaming matters here: checking `Vec::len()` after `decode_all` would be too late for a
/// compressed expansion bomb.
pub fn decode_zstd_chunk_bounded(
    encoded: &[u8],
    max_decoded_bytes: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut decoder = zstd::stream::Decoder::new(Cursor::new(encoded))?;
    read_bounded(&mut decoder, max_decoded_bytes)
}

/// Decode one LZ4 chunk using the size-prepended framing used by the current benchmark corpus.
pub fn decode_lz4_chunk(encoded: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    decode_lz4_chunk_bounded(encoded, usize::MAX)
}

/// Decode size-prepended LZ4 only after validating the advertised destination length.
pub fn decode_lz4_chunk_bounded(
    encoded: &[u8],
    max_decoded_bytes: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let Some(prefix) = encoded.get(..std::mem::size_of::<u32>()) else {
        return Err("LZ4 chunk is missing its decoded-size prefix".into());
    };
    let decoded_bytes = u32::from_le_bytes(prefix.try_into().expect("u32-sized prefix")) as usize;
    if decoded_bytes > max_decoded_bytes {
        return Err(format!(
            "LZ4 decoded size {decoded_bytes} exceeds the {max_decoded_bytes}-byte limit"
        )
        .into());
    }
    lz4_flex::decompress_size_prepended(encoded).map_err(Into::into)
}

/// Decode one Blosc chunk after validating its self-described decoded length.
pub fn decode_blosc_chunk(encoded: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    decode_blosc_chunk_bounded(encoded, usize::MAX)
}

/// Decode Blosc only after validating its self-described output size against a caller budget.
pub fn decode_blosc_chunk_bounded(
    encoded: &[u8],
    max_decoded_bytes: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let decoded_bytes = blosc_validate(encoded).ok_or("invalid Blosc buffer")?;
    if decoded_bytes > max_decoded_bytes {
        return Err(format!(
            "Blosc decoded size {decoded_bytes} exceeds the {max_decoded_bytes}-byte limit"
        )
        .into());
    }
    blosc_decompress_bytes(encoded, decoded_bytes, 1).map_err(Into::into)
}

fn read_bounded(
    reader: &mut impl Read,
    max_decoded_bytes: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut decoded = Vec::with_capacity(max_decoded_bytes.min(64 * 1024));
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let remaining = max_decoded_bytes.saturating_sub(decoded.len());
        // Ask for one extra byte at the boundary so an exact-limit chunk is accepted but an
        // over-limit stream cannot be mistaken for it.
        let read_size = buffer.len().min(remaining.saturating_add(1));
        if read_size == 0 {
            return Err(
                format!("Zstd decoded data exceeds the {max_decoded_bytes}-byte limit").into(),
            );
        }
        let read = reader.read(&mut buffer[..read_size])?;
        if read == 0 {
            return Ok(decoded);
        }
        if read > remaining {
            return Err(
                format!("Zstd decoded data exceeds the {max_decoded_bytes}-byte limit").into(),
            );
        }
        decoded.extend_from_slice(&buffer[..read]);
    }
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn wasm_benchmark_json() -> String {
    benchmark_json().unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
}

/// Runs the per-chunk benchmark on JavaScript-owned `Uint8Array` chunk bytes.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn wasm_benchmark_chunks_json(chunks: js_sys::Array) -> String {
    let chunks = chunks
        .iter()
        .map(|value| js_sys::Uint8Array::new(&value).to_vec())
        .collect::<Vec<_>>();
    benchmark_chunks_json(&chunks).unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
}

fn measure(
    codec: &str,
    encoded: &[u8],
    expected_checksum: u64,
    decode: impl Fn(&[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>>,
) -> Result<String, Box<dyn std::error::Error>> {
    for _ in 0..WARMUP_ITERATIONS {
        let decoded = decode(encoded)?;
        assert_eq!(
            checksum(&decoded),
            expected_checksum,
            "{codec} warmup checksum"
        );
        black_box(decoded);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let started = Instant::now();
    #[cfg(target_arch = "wasm32")]
    let started = wasm_now_millis();
    for _ in 0..MEASURED_ITERATIONS {
        let decoded = decode(encoded)?;
        assert_eq!(checksum(&decoded), expected_checksum, "{codec} checksum");
        black_box(decoded);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let seconds = started.elapsed().as_secs_f64();
    #[cfg(target_arch = "wasm32")]
    let seconds = (wasm_now_millis() - started) / 1_000.0;
    let mib_per_second =
        PAYLOAD_BYTES as f64 * MEASURED_ITERATIONS as f64 / seconds / 1024.0 / 1024.0;
    Ok(format!(
        "{{\"codec\":\"{codec}\",\"encodedBytes\":{},\"decodeMiBPerSecond\":{mib_per_second:.3}}}",
        encoded.len()
    ))
}

fn measure_chunks(
    codec: &str,
    encoded_chunks: &[Vec<u8>],
    expected_checksums: &[u64],
    decode: impl Fn(&[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>>,
) -> Result<String, Box<dyn std::error::Error>> {
    for _ in 0..WARMUP_ITERATIONS {
        for (encoded, expected) in encoded_chunks.iter().zip(expected_checksums) {
            let decoded = decode(encoded)?;
            assert_eq!(checksum(&decoded), *expected, "{codec} warmup checksum");
            black_box(decoded);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let started = Instant::now();
    #[cfg(target_arch = "wasm32")]
    let started = wasm_now_millis();
    let mut decoded_bytes = 0usize;
    for _ in 0..MEASURED_ITERATIONS {
        for (encoded, expected) in encoded_chunks.iter().zip(expected_checksums) {
            let decoded = decode(encoded)?;
            assert_eq!(checksum(&decoded), *expected, "{codec} checksum");
            decoded_bytes += decoded.len();
            black_box(decoded);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let seconds = started.elapsed().as_secs_f64();
    #[cfg(target_arch = "wasm32")]
    let seconds = (wasm_now_millis() - started) / 1_000.0;
    let mib_per_second = decoded_bytes as f64 / seconds / 1024.0 / 1024.0;
    let encoded_bytes: usize = encoded_chunks.iter().map(Vec::len).sum();
    Ok(format!(
        "{{\"codec\":\"{codec}\",\"encodedBytes\":{encoded_bytes},\"decodeMiBPerSecond\":{mib_per_second:.3}}}"
    ))
}

fn deterministic_payload(len: usize) -> Vec<u8> {
    let mut state = 0x6a09_e667_f3bc_c909_u64;
    (0..len)
        .map(|index| {
            // A mix of low-frequency structure and noise avoids a degenerate all-zero codec case.
            state ^= state << 7;
            state ^= state >> 9;
            ((state as u8) & 0x1f).wrapping_add(((index / 257) as u8).wrapping_mul(3))
        })
        .collect()
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x1000_0000_01b3)
    })
}

#[cfg(target_arch = "wasm32")]
fn wasm_now_millis() -> f64 {
    let global = js_sys::global();
    let performance = js_sys::Reflect::get(&global, &"performance".into()).ok();
    performance
        .and_then(|performance| {
            let now = js_sys::Reflect::get(&performance, &"now".into()).ok()?;
            let now = now.dyn_into::<js_sys::Function>().ok()?;
            now.call0(&performance).ok()?.as_f64()
        })
        .unwrap_or_else(js_sys::Date::now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codecs_round_trip_the_identical_payload() {
        let payload = deterministic_payload(16 * 1024);
        let expected = checksum(&payload);
        let zstd = zstd::stream::encode_all(Cursor::new(&payload), 3).unwrap();
        let lz4 = lz4_flex::compress_prepend_size(&payload);
        assert_eq!(
            checksum(&zstd::stream::decode_all(Cursor::new(&zstd)).unwrap()),
            expected
        );
        assert_eq!(
            checksum(&lz4_flex::decompress_size_prepended(&lz4).unwrap()),
            expected
        );
        let blosc = blosc_encode(&payload).unwrap();
        assert_eq!(checksum(&decode_zstd_chunk(&zstd).unwrap()), expected);
        assert_eq!(checksum(&decode_lz4_chunk(&lz4).unwrap()), expected);
        assert_eq!(checksum(&decode_blosc_chunk(&blosc).unwrap()), expected);
    }

    #[test]
    fn bounded_decoders_reject_expansion_before_allocating_the_full_payload() {
        let payload = deterministic_payload(16 * 1024);
        let zstd = zstd::stream::encode_all(Cursor::new(&payload), 3).unwrap();
        let lz4 = lz4_flex::compress_prepend_size(&payload);
        let blosc = blosc_encode(&payload).unwrap();
        for result in [
            decode_zstd_chunk_bounded(&zstd, 1024),
            decode_lz4_chunk_bounded(&lz4, 1024),
            decode_blosc_chunk_bounded(&blosc, 1024),
        ] {
            assert!(result.is_err());
        }
        assert_eq!(
            decode_zstd_chunk_bounded(&zstd, payload.len()).unwrap(),
            payload
        );
    }

    #[test]
    fn chunk_corpus_preserves_chunk_boundaries_and_reports_their_count() {
        let result = benchmark_chunks_json(&[vec![1, 2, 3], vec![4, 5, 6, 7]]).unwrap();
        assert!(result.contains("\"kind\":\"chunk-corpus\""));
        assert!(result.contains("\"chunkCount\":2"));
        assert!(result.contains("\"payloadBytes\":7"));
    }
}
