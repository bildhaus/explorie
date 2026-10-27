//! Newline-delimited JSON-RPC frames cross the host/plugin process boundary in
//! both directions; either side may be buggy or hostile.
#![no_main]

use explorie_plugin_protocol::{
    ActionEffect, ActionRequest, CatalogEntry, Contribution, Inspection, Manifest, read_frame,
    write_frame,
};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;
use std::io::{BufReader, Cursor};

/// serde_json's default float parser is best-effort (no `float_roundtrip`), so
/// a float may come back one ULP off; only float-free values must match exactly.
fn has_float(value: &Value) -> bool {
    match value {
        Value::Number(number) => number.is_f64(),
        Value::Array(items) => items.iter().any(has_float),
        Value::Object(map) => map.values().any(has_float),
        _ => false,
    }
}

fn decode_payload(value: &Value) {
    if let Ok(manifest) = serde_json::from_value::<Manifest>(value.clone()) {
        let _ = manifest.validate();
    }
    let _ = serde_json::from_value::<Contribution>(value.clone());
    let _ = serde_json::from_value::<Inspection>(value.clone());
    let _ = serde_json::from_value::<ActionRequest>(value.clone());
    let _ = serde_json::from_value::<ActionEffect>(value.clone());
    let _ = serde_json::from_value::<CatalogEntry>(value.clone());
}

fuzz_target!(|data: &[u8]| {
    let Some((&capacity, stream)) = data.split_first() else {
        return;
    };
    // Tiny buffers make frames straddle fill_buf() boundaries.
    let mut reader = BufReader::with_capacity(usize::from(capacity).max(1), Cursor::new(stream));
    for _ in 0..64 {
        let Ok(Some(frame)) = read_frame(&mut reader) else {
            break;
        };
        let mut encoded = Vec::new();
        write_frame(&mut encoded, &frame).expect("re-encode an accepted frame");
        let decoded = read_frame(&mut Cursor::new(encoded))
            .expect("re-read an encoded frame")
            .expect("an encoded frame is not end-of-stream");
        if !has_float(&frame) {
            assert_eq!(decoded, frame);
        }

        decode_payload(&frame);
        for key in ["params", "result"] {
            if let Some(payload) = frame.get(key) {
                decode_payload(payload);
            }
        }
    }
});
