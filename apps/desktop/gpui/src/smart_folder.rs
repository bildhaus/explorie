use std::path::PathBuf;

use chrono::NaiveDate;
use explorie_native_services::{CombineMode, SearchCriteria, SearchType};
use regex::RegexBuilder;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SmartFolderField {
    Name,
    SearchPaths,
    NamePattern,
    ExcludePattern,
    Extensions,
    ContentSearch,
    Tag,
    SizeMin,
    SizeMax,
    ModifiedAfter,
    ModifiedBefore,
}

impl SmartFolderField {
    pub(crate) const ALL: [Self; 11] = [
        Self::Name,
        Self::SearchPaths,
        Self::NamePattern,
        Self::ExcludePattern,
        Self::Extensions,
        Self::ContentSearch,
        Self::Tag,
        Self::SizeMin,
        Self::SizeMax,
        Self::ModifiedAfter,
        Self::ModifiedBefore,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::SearchPaths => "Search roots",
            Self::NamePattern => "Name pattern",
            Self::ExcludePattern => "Exclude pattern",
            Self::Extensions => "Extensions",
            Self::ContentSearch => "Contains text",
            Self::Tag => "Tag",
            Self::SizeMin => "Minimum size",
            Self::SizeMax => "Maximum size",
            Self::ModifiedAfter => "Modified after",
            Self::ModifiedBefore => "Modified before",
        }
    }

    pub(crate) fn hint(self) -> &'static str {
        match self {
            Self::Name => "Required, 1–64 characters",
            Self::SearchPaths => "One folder per line; Shift+Enter adds another root",
            Self::NamePattern => "Case-insensitive text or regex",
            Self::ExcludePattern => "Names matching this are omitted",
            Self::Extensions => "Comma-separated, for example: rs, md, txt",
            Self::ContentSearch => "Local text files up to 5 MB",
            Self::Tag => "A Finder tag name, for example: Work",
            Self::SizeMin | Self::SizeMax => "Bytes; leave empty for no limit",
            Self::ModifiedAfter | Self::ModifiedBefore => "YYYY-MM-DD; leave empty for no limit",
        }
    }

    pub(crate) fn debug_id(self) -> &'static str {
        match self {
            Self::Name => "smart-folder-field-name",
            Self::SearchPaths => "smart-folder-field-paths",
            Self::NamePattern => "smart-folder-field-name-pattern",
            Self::ExcludePattern => "smart-folder-field-exclude",
            Self::Extensions => "smart-folder-field-extensions",
            Self::ContentSearch => "smart-folder-field-content",
            Self::Tag => "smart-folder-field-tag",
            Self::SizeMin => "smart-folder-field-size-min",
            Self::SizeMax => "smart-folder-field-size-max",
            Self::ModifiedAfter => "smart-folder-field-modified-after",
            Self::ModifiedBefore => "smart-folder-field-modified-before",
        }
    }

    pub(crate) fn offset(self, delta: isize) -> Self {
        let index = Self::ALL
            .iter()
            .position(|field| *field == self)
            .expect("smart-folder field belongs to ALL");
        let next = index.saturating_add_signed(delta).min(Self::ALL.len() - 1);
        Self::ALL[next]
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SmartFolderDraft {
    pub(crate) id: Option<u64>,
    pub(crate) field: SmartFolderField,
    pub(crate) name: String,
    pub(crate) search_paths: String,
    pub(crate) name_pattern: String,
    pub(crate) exclude_pattern: String,
    pub(crate) extensions: String,
    pub(crate) content_search: String,
    pub(crate) tag: String,
    pub(crate) size_min: String,
    pub(crate) size_max: String,
    pub(crate) modified_after: String,
    pub(crate) modified_before: String,
    pub(crate) name_regex: bool,
    pub(crate) type_filter: SearchType,
    pub(crate) recursive: bool,
    pub(crate) combine_mode: CombineMode,
}

impl SmartFolderDraft {
    pub(crate) fn new(id: Option<u64>, name: String, criteria: SearchCriteria) -> Self {
        Self {
            id,
            field: SmartFolderField::Name,
            name,
            search_paths: criteria
                .search_paths
                .iter()
                .map(|path| path.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\n"),
            name_pattern: criteria.name_pattern.unwrap_or_default(),
            exclude_pattern: criteria.exclude_pattern.unwrap_or_default(),
            extensions: criteria.extensions.join(", "),
            content_search: criteria.content_search.unwrap_or_default(),
            tag: criteria.tag.unwrap_or_default(),
            size_min: optional_number(criteria.size_min),
            size_max: optional_number(criteria.size_max),
            modified_after: optional_date(criteria.modified_after),
            modified_before: optional_date(criteria.modified_before),
            name_regex: criteria.name_regex,
            type_filter: criteria.type_filter,
            recursive: criteria.recursive,
            combine_mode: criteria.combine_mode,
        }
    }

    pub(crate) fn value(&self, field: SmartFolderField) -> &str {
        match field {
            SmartFolderField::Name => &self.name,
            SmartFolderField::SearchPaths => &self.search_paths,
            SmartFolderField::NamePattern => &self.name_pattern,
            SmartFolderField::ExcludePattern => &self.exclude_pattern,
            SmartFolderField::Extensions => &self.extensions,
            SmartFolderField::ContentSearch => &self.content_search,
            SmartFolderField::Tag => &self.tag,
            SmartFolderField::SizeMin => &self.size_min,
            SmartFolderField::SizeMax => &self.size_max,
            SmartFolderField::ModifiedAfter => &self.modified_after,
            SmartFolderField::ModifiedBefore => &self.modified_before,
        }
    }

    pub(crate) fn set_value(&mut self, field: SmartFolderField, value: String) {
        match field {
            SmartFolderField::Name => self.name = value,
            SmartFolderField::SearchPaths => self.search_paths = value,
            SmartFolderField::NamePattern => self.name_pattern = value,
            SmartFolderField::ExcludePattern => self.exclude_pattern = value,
            SmartFolderField::Extensions => self.extensions = value,
            SmartFolderField::ContentSearch => self.content_search = value,
            SmartFolderField::Tag => self.tag = value,
            SmartFolderField::SizeMin => self.size_min = value,
            SmartFolderField::SizeMax => self.size_max = value,
            SmartFolderField::ModifiedAfter => self.modified_after = value,
            SmartFolderField::ModifiedBefore => self.modified_before = value,
        }
    }

    pub(crate) fn validate(&self) -> Result<(String, SearchCriteria), String> {
        let name = self.name.trim().to_string();
        if name.is_empty() || name.chars().count() > 64 {
            return Err("Smart folder name must be 1–64 characters".to_string());
        }

        let search_paths = self
            .search_paths
            .lines()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        if search_paths.is_empty() {
            return Err("Choose at least one search root".to_string());
        }
        if let Some(path) = search_paths.iter().find(|path| !path.is_dir()) {
            return Err(format!(
                "Search root is not an available folder: {}",
                path.display()
            ));
        }

        let name_pattern = optional_text(&self.name_pattern);
        let exclude_pattern = optional_text(&self.exclude_pattern);
        if self.name_regex {
            for (label, pattern) in [
                ("Name pattern", name_pattern.as_deref()),
                ("Exclude pattern", exclude_pattern.as_deref()),
            ] {
                if let Some(pattern) = pattern {
                    RegexBuilder::new(pattern)
                        .case_insensitive(true)
                        .build()
                        .map_err(|error| format!("{label} is not valid regex: {error}"))?;
                }
            }
        }

        let extensions = self
            .extensions
            .split(',')
            .map(str::trim)
            .map(|extension| extension.trim_start_matches('.').to_ascii_lowercase())
            .filter(|extension| !extension.is_empty())
            .collect::<Vec<_>>();
        if let Some(extension) = extensions
            .iter()
            .find(|extension| extension.contains(['/', '\\', ';']))
        {
            return Err(format!("Invalid extension: {extension}"));
        }

        let tag = optional_text(&self.tag);
        if tag
            .as_deref()
            .is_some_and(|tag| tag.chars().any(char::is_control))
        {
            return Err("Tag names cannot contain line breaks or control characters".to_string());
        }

        let size_min = optional_u64(&self.size_min, "Minimum size")?;
        let size_max = optional_u64(&self.size_max, "Maximum size")?;
        if matches!((size_min, size_max), (Some(minimum), Some(maximum)) if minimum > maximum) {
            return Err("Minimum size cannot exceed maximum size".to_string());
        }

        let modified_after = optional_date_millis(&self.modified_after, false)?;
        let modified_before = optional_date_millis(&self.modified_before, true)?;
        if matches!((modified_after, modified_before), (Some(after), Some(before)) if after > before)
        {
            return Err("Modified-after date cannot follow modified-before date".to_string());
        }

        Ok((
            name,
            SearchCriteria {
                name_pattern,
                name_regex: self.name_regex,
                extensions,
                type_filter: self.type_filter,
                size_min,
                size_max,
                modified_after,
                modified_before,
                content_search: optional_text(&self.content_search),
                search_paths,
                recursive: self.recursive,
                combine_mode: self.combine_mode,
                exclude_pattern,
                tag,
            },
        ))
    }
}

fn optional_text(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn optional_number(value: Option<u64>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

fn optional_u64(value: &str, label: &str) -> Result<Option<u64>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<u64>()
        .map(Some)
        .map_err(|_| format!("{label} must be a whole number of bytes"))
}

fn optional_date(value: Option<u64>) -> String {
    let Some(value) = value.and_then(|value| i64::try_from(value).ok()) else {
        return String::new();
    };
    chrono::DateTime::from_timestamp_millis(value)
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn optional_date_millis(value: &str, end_of_day: bool) -> Result<Option<u64>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| "Dates must use YYYY-MM-DD".to_string())?;
    let start = date
        .and_hms_milli_opt(0, 0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc()
        .timestamp_millis();
    let millis = if end_of_day {
        start.saturating_add(86_400_000 - 1)
    } else {
        start
    };
    u64::try_from(millis)
        .map(Some)
        .map_err(|_| "Dates before 1970 are not supported".to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use uuid::Uuid;

    fn fixture_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "explorie-smart-folder-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn complete_draft_validates_into_the_native_criteria_shape() {
        let root = fixture_dir();
        let second_root = root.join("semi;colon");
        fs::create_dir(&second_root).unwrap();
        let mut draft = SmartFolderDraft::new(
            None,
            "Recent source".to_string(),
            SearchCriteria {
                search_paths: vec![root.clone()],
                ..SearchCriteria::default()
            },
        );
        draft.name_pattern = "^(src|test)".to_string();
        draft.search_paths = format!("{}\n{}", root.display(), second_root.display());
        draft.exclude_pattern = "generated$".to_string();
        draft.extensions = ".RS, md, rs".to_string();
        draft.content_search = "needle".to_string();
        draft.tag = " Work ".to_string();
        draft.size_min = "12".to_string();
        draft.size_max = "4096".to_string();
        draft.modified_after = "2026-01-02".to_string();
        draft.modified_before = "2026-02-03".to_string();
        draft.name_regex = true;
        draft.type_filter = SearchType::Files;
        draft.recursive = false;
        draft.combine_mode = CombineMode::Or;

        let (name, criteria) = draft.validate().unwrap();
        assert_eq!(name, "Recent source");
        assert_eq!(criteria.name_pattern.as_deref(), Some("^(src|test)"));
        assert_eq!(criteria.exclude_pattern.as_deref(), Some("generated$"));
        assert_eq!(criteria.extensions, ["rs", "md", "rs"]);
        assert_eq!(criteria.content_search.as_deref(), Some("needle"));
        assert_eq!(criteria.tag.as_deref(), Some("Work"));
        assert_eq!(criteria.size_min, Some(12));
        assert_eq!(criteria.size_max, Some(4096));
        assert!(criteria.modified_before.unwrap() > criteria.modified_after.unwrap());
        assert_eq!(criteria.search_paths, [root.clone(), second_root]);
        assert_eq!(criteria.type_filter, SearchType::Files);
        assert_eq!(criteria.combine_mode, CombineMode::Or);
        assert!(!criteria.recursive);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_drafts_keep_specific_recoverable_errors() {
        let root = fixture_dir();
        let mut draft = SmartFolderDraft::new(
            None,
            String::new(),
            SearchCriteria {
                search_paths: vec![root.clone()],
                ..SearchCriteria::default()
            },
        );
        assert_eq!(
            draft.validate().unwrap_err(),
            "Smart folder name must be 1–64 characters"
        );

        draft.name = "Broken".to_string();
        draft.name_regex = true;
        draft.name_pattern = "[".to_string();
        assert!(
            draft
                .validate()
                .unwrap_err()
                .starts_with("Name pattern is not valid regex")
        );

        draft.name_regex = false;
        draft.name_pattern.clear();
        draft.size_min = "20".to_string();
        draft.size_max = "10".to_string();
        assert_eq!(
            draft.validate().unwrap_err(),
            "Minimum size cannot exceed maximum size"
        );

        draft.size_min.clear();
        draft.size_max.clear();
        draft.modified_after = "tomorrow".to_string();
        assert_eq!(draft.validate().unwrap_err(), "Dates must use YYYY-MM-DD");

        draft.modified_after.clear();
        draft.tag = "Two\nLines".to_string();
        assert_eq!(
            draft.validate().unwrap_err(),
            "Tag names cannot contain line breaks or control characters"
        );

        draft.tag = "   ".to_string();
        assert_eq!(draft.validate().unwrap().1.tag, None);

        draft.search_paths = root.join("missing").to_string_lossy().into_owned();
        assert!(
            draft
                .validate()
                .unwrap_err()
                .starts_with("Search root is not an available folder")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn field_navigation_clamps_at_both_ends() {
        assert_eq!(SmartFolderField::Name.offset(-1), SmartFolderField::Name);
        assert_eq!(
            SmartFolderField::Name.offset(1),
            SmartFolderField::SearchPaths
        );
        assert_eq!(
            SmartFolderField::ModifiedBefore.offset(1),
            SmartFolderField::ModifiedBefore
        );
    }
}
