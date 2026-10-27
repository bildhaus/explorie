//! `.explorie.json` travels with folders (downloads, shares, archives), so its
//! custom fields are untrusted when a directory is listed or edited.
#![no_main]

use explorie_core::fuzzing::parse_explorie_document;
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

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

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(document) = parse_explorie_document(text) else {
        return;
    };
    // Anything that validates must survive the atomic writer's round trip, or
    // the next metadata edit would corrupt or reject the user's file.
    let written = serde_json::to_vec_pretty(&document).expect("serialize validated metadata");
    let written = std::str::from_utf8(&written).expect("serde_json writes UTF-8");
    let reread = parse_explorie_document(written).expect("written metadata must reload");
    let exact = !document
        .values()
        .flat_map(|fields| fields.values())
        .any(has_float);
    if exact {
        assert_eq!(reread, document);
    } else {
        assert_eq!(reread.len(), document.len());
    }
});
