use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use explorie_core::FileEntry;
use serde_json::{Number, Value};

pub const FIELD_SUGGESTIONS: [&str; 8] = [
    "status", "priority", "type", "category", "project", "tags", "notes", "dueDate",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CustomFieldInput {
    Name,
    Value,
}

#[derive(Clone, Debug)]
pub struct CustomFieldDraft {
    pub original_name: Option<String>,
    pub active: CustomFieldInput,
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct CustomFieldsEditor {
    pub path: PathBuf,
    pub fields: BTreeMap<String, Value>,
    pub draft: Option<CustomFieldDraft>,
    pub pending: bool,
}

impl CustomFieldsEditor {
    pub fn new(entry: &FileEntry) -> Self {
        Self {
            path: entry.path.clone(),
            fields: entry
                .custom
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
            draft: None,
            pending: false,
        }
    }

    pub fn begin_add(&mut self) {
        self.draft = Some(CustomFieldDraft {
            original_name: None,
            active: CustomFieldInput::Name,
            name: String::new(),
            value: String::new(),
        });
    }

    pub fn begin_edit(&mut self, name: &str) -> bool {
        let Some(value) = self.fields.get(name) else {
            return false;
        };
        self.draft = Some(CustomFieldDraft {
            original_name: Some(name.to_string()),
            active: CustomFieldInput::Value,
            name: name.to_string(),
            value: display_value(value),
        });
        true
    }

    pub fn cancel(&mut self) {
        self.draft = None;
    }

    pub fn active_value(&self) -> &str {
        let Some(draft) = &self.draft else {
            return "";
        };
        match draft.active {
            CustomFieldInput::Name => &draft.name,
            CustomFieldInput::Value => &draft.value,
        }
    }

    pub fn set_active_value(&mut self, value: String) {
        let Some(draft) = &mut self.draft else {
            return;
        };
        match draft.active {
            CustomFieldInput::Name => draft.name = value,
            CustomFieldInput::Value => draft.value = value,
        }
    }

    pub fn toggle_input(&mut self) {
        if let Some(draft) = &mut self.draft {
            draft.active = match draft.active {
                CustomFieldInput::Name => CustomFieldInput::Value,
                CustomFieldInput::Value => CustomFieldInput::Name,
            };
        }
    }

    pub fn set_active_input(&mut self, input: CustomFieldInput) {
        if let Some(draft) = &mut self.draft {
            draft.active = input;
        }
    }

    pub fn proposed_fields(&self) -> Result<HashMap<String, Value>, String> {
        let draft = self
            .draft
            .as_ref()
            .ok_or_else(|| "Choose Add field or Edit first".to_string())?;
        let name = draft.name.trim();
        if name.is_empty() {
            return Err("Custom field names cannot be empty".to_string());
        }
        if name.len() > 128 || name.chars().any(char::is_control) {
            return Err("Custom field names must be 128 characters or fewer".to_string());
        }
        if draft.original_name.as_deref() != Some(name) && self.fields.contains_key(name) {
            return Err(format!("A custom field named {name} already exists"));
        }
        let previous = draft
            .original_name
            .as_deref()
            .and_then(|original| self.fields.get(original));
        let value = parse_value(name, &draft.value, previous)?;
        let mut fields: HashMap<_, _> = self
            .fields
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        if let Some(original) = &draft.original_name {
            fields.remove(original);
        }
        fields.insert(name.to_string(), value);
        Ok(fields)
    }

    pub fn fields_without(&self, name: &str) -> HashMap<String, Value> {
        self.fields
            .iter()
            .filter(|(field, _)| field.as_str() != name)
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }

    pub fn apply_saved(&mut self, fields: HashMap<String, Value>) {
        self.fields = fields.into_iter().collect();
        self.draft = None;
        self.pending = false;
    }
}

pub fn display_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(_) => "Unsupported object".to_string(),
    }
}

fn parse_value(name: &str, input: &str, previous: Option<&Value>) -> Result<Value, String> {
    if name.eq_ignore_ascii_case("tags") || matches!(previous, Some(Value::Array(_))) {
        let mut tags = Vec::new();
        for tag in input
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
        {
            if tag.len() > 4 * 1024 {
                return Err("Custom field tags are limited to 4 KiB each".to_string());
            }
            if !tags.iter().any(|existing| existing == tag) {
                tags.push(tag.to_string());
            }
        }
        if tags.len() > 256 {
            return Err("Custom fields are limited to 256 tags".to_string());
        }
        return Ok(Value::Array(tags.into_iter().map(Value::String).collect()));
    }
    if input.len() > 16 * 1024 {
        return Err("Custom field values are limited to 16 KiB".to_string());
    }
    match previous {
        Some(Value::Bool(_)) => input
            .parse::<bool>()
            .map(Value::Bool)
            .map_err(|_| "Boolean fields accept only true or false".to_string()),
        Some(Value::Number(_)) => input
            .parse::<Number>()
            .map(Value::Number)
            .map_err(|_| "Number fields require a valid JSON number".to_string()),
        Some(Value::Null) if input == "null" => Ok(Value::Null),
        _ => Ok(Value::String(input.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    use uuid::Uuid;

    fn entry() -> FileEntry {
        FileEntry {
            id: Uuid::new_v4(),
            path: PathBuf::from("C:/files/report.txt"),
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
            hidden: false,
            is_dir: false,
            custom: HashMap::from([
                ("rating".to_string(), Value::from(5)),
                ("tags".to_string(), Value::from(vec!["work"])),
            ]),
            is_symlink: false,
            is_junction: false,
            link_target: None,
            has_xattrs: false,
            is_package: false,
            link_target_is_dir: false,
            is_cloud_placeholder: false,
            tags: Vec::new(),
        }
    }

    #[test]
    fn add_edit_remove_and_type_preservation_are_deterministic() {
        let mut editor = CustomFieldsEditor::new(&entry());
        editor.begin_add();
        editor.set_active_value("status".to_string());
        editor.toggle_input();
        editor.set_active_value("Done".to_string());
        let fields = editor.proposed_fields().unwrap();
        assert_eq!(fields["status"], "Done");
        editor.apply_saved(fields);

        assert!(editor.begin_edit("tags"));
        editor.set_active_value("work, review, work".to_string());
        let fields = editor.proposed_fields().unwrap();
        assert_eq!(fields["tags"], serde_json::json!(["work", "review"]));
        editor.apply_saved(fields);

        assert!(editor.begin_edit("rating"));
        editor.set_active_value("not a number".to_string());
        assert!(editor.proposed_fields().is_err());
        assert!(!editor.fields_without("status").contains_key("status"));
    }
}
