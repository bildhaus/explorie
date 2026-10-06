use crate::{
    ActiveOperation, BlockingTask, ErrorCode, ServiceContext, ServiceError, ServiceResult,
    SharedState,
};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
pub struct MetadataService {
    context: ServiceContext,
    shared: Arc<SharedState>,
}

impl MetadataService {
    pub(crate) fn new(context: ServiceContext, shared: Arc<SharedState>) -> Self {
        Self { context, shared }
    }

    pub fn create_schema(
        &self,
        dir_path: PathBuf,
        fields: HashMap<String, HashMap<String, Value>>,
    ) -> BlockingTask<()> {
        let shared = Arc::clone(&self.shared);
        let guard = ActiveOperation::new(Arc::clone(&shared));
        self.context.spawn_blocking(move || {
            if remote_root(&shared, &dir_path) {
                return Err(remote_root_error());
            }
            let _guard = guard;
            explorie_core::create_explorie_schema(&dir_path, fields).map_err(ServiceError::from)
        })
    }

    pub fn create_schema_blocking(
        &self,
        dir_path: &Path,
        fields: HashMap<String, HashMap<String, Value>>,
    ) -> ServiceResult<()> {
        if remote_root(&self.shared, dir_path) {
            return Err(remote_root_error());
        }
        let _guard = ActiveOperation::new(Arc::clone(&self.shared));
        explorie_core::create_explorie_schema(dir_path, fields).map_err(ServiceError::from)
    }

    pub fn update_fields(
        &self,
        dir_path: PathBuf,
        file_name: String,
        custom_fields: HashMap<String, Value>,
    ) -> BlockingTask<()> {
        let shared = Arc::clone(&self.shared);
        let guard = ActiveOperation::new(Arc::clone(&shared));
        self.context.spawn_blocking(move || {
            if remote_root(&shared, &dir_path) {
                return Err(remote_root_error());
            }
            let _guard = guard;
            validate_update(&dir_path, &file_name, &custom_fields).map_err(ServiceError::from)?;
            explorie_core::update_custom_fields(&dir_path, &file_name, custom_fields)
                .map_err(ServiceError::from)
        })
    }

    pub fn update_fields_blocking(
        &self,
        dir_path: &Path,
        file_name: &str,
        custom_fields: HashMap<String, Value>,
    ) -> ServiceResult<()> {
        if remote_root(&self.shared, dir_path) {
            return Err(remote_root_error());
        }
        let _guard = ActiveOperation::new(Arc::clone(&self.shared));
        validate_update(dir_path, file_name, &custom_fields).map_err(ServiceError::from)?;
        explorie_core::update_custom_fields(dir_path, file_name, custom_fields)
            .map_err(ServiceError::from)
    }
}

fn validate_update(
    dir_path: &Path,
    file_name: &str,
    custom_fields: &HashMap<String, Value>,
) -> io::Result<()> {
    let directory = fs::symlink_metadata(dir_path)?;
    if directory.file_type().is_symlink() || !directory.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Custom metadata parent must be a real directory",
        ));
    }
    let relative = Path::new(file_name);
    if file_name.is_empty()
        || file_name == "."
        || file_name == ".."
        || relative.components().count() != 1
        || relative.file_name().is_none()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Custom metadata file name must be one leaf name",
        ));
    }
    fs::symlink_metadata(dir_path.join(relative))?;
    if custom_fields.len() > 128 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Custom metadata is limited to 128 fields per item",
        ));
    }
    for (name, value) in custom_fields {
        if name.trim().is_empty()
            || name.len() > 128
            || name.chars().any(char::is_control)
            || !valid_custom_value(value)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid custom metadata field: {name}"),
            ));
        }
    }
    let encoded = serde_json::to_vec(custom_fields)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if encoded.len() > 256 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Custom metadata is limited to 256 KiB per item",
        ));
    }
    Ok(())
}

fn valid_custom_value(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
        Value::String(value) => value.len() <= 16 * 1024,
        Value::Array(values) => {
            values.len() <= 256
                && values
                    .iter()
                    .all(|value| value.as_str().is_some_and(|value| value.len() <= 4 * 1024))
        }
        Value::Object(_) => false,
    }
}

fn remote_root(shared: &SharedState, path: &Path) -> bool {
    shared
        .remote_roots
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&crate::listing::normalize_path(path))
}

fn remote_root_error() -> ServiceError {
    ServiceError::new(
        ErrorCode::RemoteUnavailable,
        "Refusing to mutate a managed remote-drive root",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NativeServices, ResourcePaths};
    use serde_json::json;

    #[test]
    fn custom_metadata_updates_require_a_real_item_and_supported_values() {
        let temp = tempfile::tempdir().unwrap();
        let item = temp.path().join("report.txt");
        fs::write(&item, "report").unwrap();
        let services = NativeServices::new(ResourcePaths::test(temp.path()));

        services
            .metadata
            .update_fields(
                temp.path().to_path_buf(),
                "report.txt".to_string(),
                HashMap::from([
                    ("status".to_string(), json!("Done")),
                    ("tags".to_string(), json!(["work", "review"])),
                ]),
            )
            .wait()
            .unwrap();
        let schema: Value =
            serde_json::from_slice(&fs::read(temp.path().join(".explorie.json")).unwrap()).unwrap();
        assert_eq!(schema["report.txt"]["status"], "Done");
        assert_eq!(schema["report.txt"]["tags"], json!(["work", "review"]));

        assert!(
            services
                .metadata
                .update_fields(
                    temp.path().to_path_buf(),
                    "missing.txt".to_string(),
                    HashMap::new(),
                )
                .wait()
                .is_err()
        );
        assert!(
            services
                .metadata
                .update_fields(
                    temp.path().to_path_buf(),
                    "report.txt".to_string(),
                    HashMap::from([("nested".to_string(), json!({ "no": "objects" }))]),
                )
                .wait()
                .is_err()
        );
    }
}
