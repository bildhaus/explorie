use explorie_core::{
    create_explorie_schema, list_dir, list_dir_with_warnings, update_custom_fields,
};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Barrier};
use tempfile::tempdir;

#[test]
fn list_dir_includes_custom_fields_from_metadata_file() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();

    fs::write(root.join("notes.txt"), b"notes").unwrap();
    let metadata = json!({
        "notes.txt": {
            "tag": "docs",
            "priority": 3
        },
        "other.txt": {
            "tag": "unused"
        }
    });
    fs::write(
        root.join(".explorie.json"),
        serde_json::to_string_pretty(&metadata).unwrap(),
    )
    .unwrap();

    let entries = list_dir(root).expect("list dir");
    assert_eq!(entries.len(), 1, "metadata file should not be listed");

    let entry = entries
        .iter()
        .find(|entry| entry.path.file_name().unwrap() == "notes.txt")
        .expect("notes.txt entry");
    assert_eq!(entry.custom.get("tag"), Some(&json!("docs")));
    assert_eq!(entry.custom.get("priority"), Some(&json!(3)));
}

#[test]
fn update_custom_fields_persists_metadata_and_listing() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();

    fs::write(root.join("clip.mov"), b"").unwrap();

    let mut custom_fields = HashMap::new();
    custom_fields.insert("label".to_string(), json!("video"));
    custom_fields.insert("rating".to_string(), json!(5));
    update_custom_fields(root, "clip.mov", custom_fields).unwrap();

    let metadata_path = root.join(".explorie.json");
    let metadata_text = fs::read_to_string(&metadata_path).unwrap();
    let metadata_json: serde_json::Value = serde_json::from_str(&metadata_text).unwrap();
    let saved = metadata_json
        .get("clip.mov")
        .and_then(|value| value.as_object())
        .expect("entry persisted");
    assert_eq!(saved.get("label"), Some(&json!("video")));
    assert_eq!(saved.get("rating"), Some(&json!(5)));

    let entries = list_dir(root).expect("list dir");
    let entry = entries
        .iter()
        .find(|entry| entry.path.file_name().unwrap() == "clip.mov")
        .expect("clip.mov entry");
    assert_eq!(entry.custom.get("label"), Some(&json!("video")));
    assert_eq!(entry.custom.get("rating"), Some(&json!(5)));
}

#[test]
fn malformed_metadata_is_reported_and_never_overwritten() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    let metadata_path = root.join(".explorie.json");
    fs::write(root.join("notes.txt"), b"notes").unwrap();
    fs::write(&metadata_path, b"{ definitely not valid json").unwrap();

    // Browsing still works: the folder lists without custom fields and the
    // problem is surfaced as a warning instead of failing the whole listing.
    let listing = list_dir_with_warnings(root, false).unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert!(listing.entries[0].custom.is_empty());
    assert_eq!(listing.warnings.len(), 1, "{:?}", listing.warnings);
    assert!(listing.warnings[0].contains(".explorie.json"));
    assert_eq!(list_dir(root).unwrap().len(), 1);

    // Writers still refuse to replace the malformed document.
    let mut fields = HashMap::new();
    fields.insert("tag".to_string(), json!("docs"));
    assert_eq!(
        update_custom_fields(root, "notes.txt", fields)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    assert_eq!(
        create_explorie_schema(root, HashMap::new())
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    let mut replacement = HashMap::new();
    replacement.insert(
        "notes.txt".to_string(),
        HashMap::from([("tag".to_string(), json!("docs"))]),
    );
    assert_eq!(
        create_explorie_schema(root, replacement)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    assert_eq!(
        fs::read_to_string(metadata_path).unwrap(),
        "{ definitely not valid json"
    );
}

#[test]
fn concurrent_metadata_updates_preserve_every_entry() {
    let temp_dir = tempdir().expect("temp dir");
    let root = Arc::new(temp_dir.path().to_path_buf());
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();

    for name in ["one.txt", "two.txt"] {
        let root = Arc::clone(&root);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            let mut fields = HashMap::new();
            fields.insert("name".to_string(), json!(name));
            update_custom_fields(&root, name, fields).unwrap();
        }));
    }
    barrier.wait();
    for thread in threads {
        thread.join().unwrap();
    }

    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join(".explorie.json")).unwrap()).unwrap();
    assert_eq!(metadata["one.txt"]["name"], json!("one.txt"));
    assert_eq!(metadata["two.txt"]["name"], json!("two.txt"));
}

#[test]
fn schema_replacement_invalidates_the_listing_cache() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::write(root.join("notes.txt"), b"notes").unwrap();

    let mut first = HashMap::new();
    first.insert(
        "notes.txt".to_string(),
        HashMap::from([("status".to_string(), json!("Todo"))]),
    );
    create_explorie_schema(root, first).unwrap();
    assert_eq!(list_dir(root).unwrap()[0].custom["status"], json!("Todo"));

    let mut second = HashMap::new();
    second.insert(
        "notes.txt".to_string(),
        HashMap::from([("status".to_string(), json!("Done"))]),
    );
    create_explorie_schema(root, second).unwrap();
    assert_eq!(list_dir(root).unwrap()[0].custom["status"], json!("Done"));
}

#[test]
fn typed_date_url_enum_values_are_accepted() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::write(root.join("notes.txt"), b"notes").unwrap();

    let mut created = HashMap::new();
    created.insert(
        "notes.txt".to_string(),
        HashMap::from([
            ("dueDate".to_string(), json!("2024-02-29")),
            ("url".to_string(), json!("https://example.com/docs")),
            ("status".to_string(), json!("In Progress")),
            ("priority".to_string(), json!("High")),
            ("type".to_string(), json!("Document")),
            ("category".to_string(), json!("Work")),
            ("label".to_string(), json!("freeform")),
        ]),
    );
    create_explorie_schema(root, created).unwrap();

    let mut updated = HashMap::new();
    updated.insert("dueDate".to_string(), json!("2024-03-01"));
    updated.insert("url".to_string(), json!("http://localhost:3000/path"));
    updated.insert("status".to_string(), json!("Done"));
    updated.insert("priority".to_string(), json!("Urgent"));
    updated.insert("type".to_string(), json!("Code"));
    updated.insert("category".to_string(), json!("Project"));
    updated.insert("rating".to_string(), json!(5));
    update_custom_fields(root, "notes.txt", updated).unwrap();

    let entries = list_dir(root).expect("list dir");
    let entry = entries
        .iter()
        .find(|entry| entry.path.file_name().unwrap() == "notes.txt")
        .expect("notes.txt entry");
    assert_eq!(entry.custom.get("dueDate"), Some(&json!("2024-03-01")));
    assert_eq!(
        entry.custom.get("url"),
        Some(&json!("http://localhost:3000/path"))
    );
    assert_eq!(entry.custom.get("status"), Some(&json!("Done")));
    assert_eq!(entry.custom.get("priority"), Some(&json!("Urgent")));
    assert_eq!(entry.custom.get("type"), Some(&json!("Code")));
    assert_eq!(entry.custom.get("category"), Some(&json!("Project")));
    assert_eq!(entry.custom.get("rating"), Some(&json!(5)));
}

#[test]
fn invalid_date_url_enum_values_are_rejected() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::write(root.join("notes.txt"), b"notes").unwrap();
    let metadata_path = root.join(".explorie.json");

    let mut initial = HashMap::new();
    initial.insert(
        "notes.txt".to_string(),
        HashMap::from([("label".to_string(), json!("kept"))]),
    );
    create_explorie_schema(root, initial).unwrap();
    let original = fs::read_to_string(&metadata_path).unwrap();

    let invalid_cases = [
        ("dueDate", json!("not-a-date")),
        ("dueDate", json!("2024-13-01")),
        ("dueDate", json!("2023-02-29")),
        ("dueDate", json!("01/01/2024")),
        ("url", json!("example.com")),
        ("url", json!("javascript:alert(1)")),
        ("url", json!("http://")),
        ("status", json!("Later")),
        ("priority", json!("Critical")),
        ("type", json!("Design")),
        ("category", json!("Other")),
    ];

    for (field, value) in invalid_cases {
        let mut fields = HashMap::new();
        fields.insert(field.to_string(), value.clone());
        let error = update_custom_fields(root, "notes.txt", fields).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{field}");
        assert!(
            error.to_string().contains(field),
            "error should name {field}: {error}"
        );

        let mut document = HashMap::new();
        document.insert(
            "notes.txt".to_string(),
            HashMap::from([(field.to_string(), value)]),
        );
        let error = create_explorie_schema(root, document).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{field}");
        assert_eq!(fs::read_to_string(&metadata_path).unwrap(), original);
    }
}

#[cfg(unix)]
#[test]
fn listings_and_directory_info_do_not_follow_symlinks() {
    use std::os::unix::fs::symlink;

    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    let data = root.join("data");
    fs::create_dir(&data).unwrap();
    fs::write(data.join("file.txt"), b"abc").unwrap();
    symlink(root, data.join("loop")).unwrap();
    symlink(root.join("missing"), root.join("dangling")).unwrap();

    let entries = list_dir(root).unwrap();
    let dangling = entries
        .iter()
        .find(|entry| entry.path.ends_with("dangling"))
        .unwrap();
    assert!(dangling.is_symlink);
    assert!(!dangling.is_dir);

    let (count, size) = explorie_core::dir_info(root).unwrap();
    assert_eq!(count, 4);
    assert_eq!(size, 3);
}
