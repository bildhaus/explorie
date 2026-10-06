use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Datelike, Local, Timelike};
use explorie_core::FileEntry;
use explorie_native_services::BatchRenameItem;
use regex::RegexBuilder;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchRenameMode {
    Replace,
    Regex,
    Number,
    Case,
    PrefixSuffix,
    DateTime,
}

impl BatchRenameMode {
    pub const ALL: [Self; 6] = [
        Self::Replace,
        Self::Regex,
        Self::Number,
        Self::Case,
        Self::PrefixSuffix,
        Self::DateTime,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Replace => "Replace",
            Self::Regex => "Regex",
            Self::Number => "Number",
            Self::Case => "Case",
            Self::PrefixSuffix => "Prefix / suffix",
            Self::DateTime => "Date / time",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertPosition {
    Prefix,
    Suffix,
    Replace,
}

impl InsertPosition {
    pub fn next(self) -> Self {
        match self {
            Self::Prefix => Self::Suffix,
            Self::Suffix => Self::Replace,
            Self::Replace => Self::Prefix,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Prefix => "prefix",
            Self::Suffix => "suffix",
            Self::Replace => "replace",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaseMode {
    Upper,
    Lower,
    Title,
    Sentence,
}

impl CaseMode {
    pub fn next(self) -> Self {
        match self {
            Self::Upper => Self::Lower,
            Self::Lower => Self::Title,
            Self::Title => Self::Sentence,
            Self::Sentence => Self::Upper,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Upper => "UPPER",
            Self::Lower => "lower",
            Self::Title => "Title Case",
            Self::Sentence => "Sentence case",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaseTarget {
    Name,
    Extension,
    Both,
}

impl CaseTarget {
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::Extension,
            Self::Extension => Self::Both,
            Self::Both => Self::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Extension => "extension",
            Self::Both => "name + extension",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DateSource {
    Now,
    Modified,
}

impl DateSource {
    pub fn next(self) -> Self {
        match self {
            Self::Now => Self::Modified,
            Self::Modified => Self::Now,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Now => "current time",
            Self::Modified => "modified time",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchRenameField {
    Primary,
    Secondary,
}

#[derive(Clone, Debug)]
pub struct BatchRenameSource {
    pub path: PathBuf,
    pub name: String,
    pub modified: SystemTime,
}

impl From<&FileEntry> for BatchRenameSource {
    fn from(entry: &FileEntry) -> Self {
        Self {
            path: entry.path.clone(),
            name: entry
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            modified: entry.modified,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BatchRenamePreview {
    pub source_path: PathBuf,
    pub original_name: String,
    pub new_name: String,
    pub changed: bool,
    pub conflict: bool,
    pub invalid_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct BatchRenameEditor {
    pub sources: Vec<BatchRenameSource>,
    pub mode: BatchRenameMode,
    pub field: BatchRenameField,
    pub primary: String,
    pub secondary: String,
    pub replace_all: bool,
    pub regex_case_insensitive: bool,
    pub regex_multiline: bool,
    pub number_start: u32,
    pub number_digits: u8,
    pub position: InsertPosition,
    pub case_mode: CaseMode,
    pub case_target: CaseTarget,
    pub date_source: DateSource,
}

impl BatchRenameEditor {
    pub fn new(entries: &[FileEntry]) -> Self {
        Self {
            sources: entries.iter().map(BatchRenameSource::from).collect(),
            mode: BatchRenameMode::Replace,
            field: BatchRenameField::Primary,
            primary: String::new(),
            secondary: String::new(),
            replace_all: true,
            regex_case_insensitive: true,
            regex_multiline: false,
            number_start: 1,
            number_digits: 3,
            position: InsertPosition::Suffix,
            case_mode: CaseMode::Lower,
            case_target: CaseTarget::Name,
            date_source: DateSource::Now,
        }
    }

    pub fn set_mode(&mut self, mode: BatchRenameMode) {
        self.mode = mode;
        self.field = BatchRenameField::Primary;
        self.primary = match mode {
            BatchRenameMode::DateTime => "YYYY-MM-DD".to_string(),
            BatchRenameMode::Number => "_".to_string(),
            _ => String::new(),
        };
        self.secondary = if mode == BatchRenameMode::DateTime {
            "_".to_string()
        } else {
            String::new()
        };
    }

    pub fn field_label(&self) -> &'static str {
        match (self.mode, self.field) {
            (BatchRenameMode::Replace, BatchRenameField::Primary) => "Find",
            (BatchRenameMode::Replace, BatchRenameField::Secondary) => "Replace with",
            (BatchRenameMode::Regex, BatchRenameField::Primary) => "Regular expression",
            (BatchRenameMode::Regex, BatchRenameField::Secondary) => "Replacement",
            (BatchRenameMode::Number, _) => "Separator",
            (BatchRenameMode::PrefixSuffix, BatchRenameField::Primary) => "Prefix",
            (BatchRenameMode::PrefixSuffix, BatchRenameField::Secondary) => "Suffix",
            (BatchRenameMode::DateTime, BatchRenameField::Primary) => "Date format",
            (BatchRenameMode::DateTime, BatchRenameField::Secondary) => "Separator",
            (BatchRenameMode::Case, _) => "No text input",
        }
    }

    pub fn active_value(&self) -> &str {
        match self.field {
            BatchRenameField::Primary => &self.primary,
            BatchRenameField::Secondary => &self.secondary,
        }
    }

    pub fn set_active_value(&mut self, value: String) {
        match self.field {
            BatchRenameField::Primary => self.primary = value,
            BatchRenameField::Secondary => self.secondary = value,
        }
    }

    pub fn toggle_field(&mut self) {
        self.field = match self.field {
            BatchRenameField::Primary => BatchRenameField::Secondary,
            BatchRenameField::Secondary => BatchRenameField::Primary,
        };
    }

    pub fn preview(&self) -> Vec<BatchRenamePreview> {
        let regex = (self.mode == BatchRenameMode::Regex && !self.primary.is_empty()).then(|| {
            RegexBuilder::new(&self.primary)
                .case_insensitive(self.regex_case_insensitive)
                .multi_line(self.regex_multiline)
                .build()
        });
        let regex_error = regex
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .map(ToString::to_string);
        let regex = regex.and_then(Result::ok);
        let mut used = HashSet::with_capacity(self.sources.len());
        self.sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let (base, extension) = split_name_extension(&source.name);
                let (mut new_base, mut new_extension) = (base.to_string(), extension.to_string());
                match self.mode {
                    BatchRenameMode::Replace if !self.primary.is_empty() => {
                        new_base = if self.replace_all {
                            base.replace(&self.primary, &self.secondary)
                        } else {
                            base.replacen(&self.primary, &self.secondary, 1)
                        };
                    }
                    BatchRenameMode::Regex => {
                        if let Some(regex) = &regex {
                            new_base = if self.replace_all {
                                regex
                                    .replace_all(base, self.secondary.as_str())
                                    .into_owned()
                            } else {
                                regex.replace(base, self.secondary.as_str()).into_owned()
                            };
                        }
                    }
                    BatchRenameMode::Number => {
                        let number = format!(
                            "{:0width$}",
                            self.number_start.saturating_add(index as u32),
                            width = usize::from(self.number_digits)
                        );
                        new_base = insert_value(base, &number, &self.primary, self.position);
                    }
                    BatchRenameMode::Case => {
                        if matches!(self.case_target, CaseTarget::Name | CaseTarget::Both) {
                            new_base = transform_case(base, self.case_mode);
                        }
                        if matches!(self.case_target, CaseTarget::Extension | CaseTarget::Both) {
                            new_extension = transform_case(extension, self.case_mode);
                        }
                    }
                    BatchRenameMode::PrefixSuffix => {
                        new_base = format!("{}{base}{}", self.primary, self.secondary);
                    }
                    BatchRenameMode::DateTime => {
                        let time = match self.date_source {
                            DateSource::Now => SystemTime::now(),
                            DateSource::Modified => source.modified,
                        };
                        let value = format_date(time, &self.primary);
                        new_base = insert_value(base, &value, &self.secondary, self.position);
                    }
                    BatchRenameMode::Replace => {}
                }
                let new_name = format!("{new_base}{new_extension}");
                let changed = new_name != source.name;
                let invalid_reason = regex_error
                    .clone()
                    .or_else(|| changed.then(|| invalid_name_reason(&new_name)).flatten());
                let conflict = !used.insert(new_name.to_ascii_lowercase());
                BatchRenamePreview {
                    source_path: source.path.clone(),
                    original_name: source.name.clone(),
                    new_name,
                    changed,
                    conflict,
                    invalid_reason,
                }
            })
            .collect()
    }

    pub fn request(&self) -> Result<Vec<BatchRenameItem>, String> {
        let preview = self.preview();
        if let Some(item) = preview
            .iter()
            .find(|item| item.conflict || item.invalid_reason.is_some())
        {
            return Err(item.invalid_reason.clone().unwrap_or_else(|| {
                format!("More than one item would be named {}", item.new_name)
            }));
        }
        let request: Vec<_> = preview
            .into_iter()
            .filter(|item| item.changed)
            .map(|item| BatchRenameItem {
                source_path: item.source_path,
                new_base_name: item.new_name,
            })
            .collect();
        if request.is_empty() {
            return Err("Batch rename does not change any selected item".to_string());
        }
        Ok(request)
    }
}

fn split_name_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(index) if index > 0 => name.split_at(index),
        _ => (name, ""),
    }
}

fn insert_value(base: &str, value: &str, separator: &str, position: InsertPosition) -> String {
    match position {
        InsertPosition::Prefix => format!("{value}{separator}{base}"),
        InsertPosition::Suffix => format!("{base}{separator}{value}"),
        InsertPosition::Replace => value.to_string(),
    }
}

fn transform_case(value: &str, mode: CaseMode) -> String {
    match mode {
        CaseMode::Upper => value.to_uppercase(),
        CaseMode::Lower => value.to_lowercase(),
        CaseMode::Title => value
            .split_inclusive(|character: char| !character.is_alphanumeric())
            .map(|word| {
                let mut characters = word.chars();
                characters.next().map_or_else(String::new, |first| {
                    first.to_uppercase().chain(characters).collect()
                })
            })
            .collect(),
        CaseMode::Sentence => {
            let mut characters = value.chars();
            characters.next().map_or_else(String::new, |first| {
                first
                    .to_uppercase()
                    .chain(characters.flat_map(char::to_lowercase))
                    .collect()
            })
        }
    }
}

fn format_date(time: SystemTime, pattern: &str) -> String {
    let date: DateTime<Local> = time.into();
    let hour_12 = date.hour() % 12;
    let hour_12 = if hour_12 == 0 { 12 } else { hour_12 };
    let values = [
        ("YYYY", format!("{:04}", date.year())),
        ("YY", format!("{:02}", date.year().rem_euclid(100))),
        ("MM", format!("{:02}", date.month())),
        ("M", date.month().to_string()),
        ("DD", format!("{:02}", date.day())),
        ("D", date.day().to_string()),
        ("HH", format!("{:02}", date.hour())),
        ("H", date.hour().to_string()),
        ("hh", format!("{hour_12:02}")),
        ("h", hour_12.to_string()),
        ("mm", format!("{:02}", date.minute())),
        ("m", date.minute().to_string()),
        ("ss", format!("{:02}", date.second())),
        ("s", date.second().to_string()),
        ("A", if date.hour() >= 12 { "PM" } else { "AM" }.to_string()),
        ("a", if date.hour() >= 12 { "pm" } else { "am" }.to_string()),
    ];
    let mut output = String::with_capacity(pattern.len() + 8);
    let mut index = 0;
    while index < pattern.len() {
        let remaining = &pattern[index..];
        if let Some((token, value)) = values
            .iter()
            .find(|(token, _)| remaining.starts_with(token))
        {
            output.push_str(value);
            index += token.len();
        } else {
            let character = remaining.chars().next().expect("non-empty date pattern");
            output.push(character);
            index += character.len_utf8();
        }
    }
    output
}

fn invalid_name_reason(name: &str) -> Option<String> {
    if name.is_empty() || name == "." || name == ".." {
        return Some("File names cannot be empty, . or ..".to_string());
    }
    if name.ends_with([' ', '.'])
        || name.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
        })
    {
        return Some(format!("Invalid file name: {name}"));
    }
    let base = Path::new(name)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(name)
        .trim_end_matches('.')
        .to_ascii_uppercase();
    if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && matches!(base.as_bytes()[3], b'1'..=b'9'))
    {
        return Some(format!("Reserved file name: {name}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use uuid::Uuid;

    fn entry(name: &str) -> FileEntry {
        FileEntry {
            id: Uuid::new_v4(),
            path: PathBuf::from("C:/batch").join(name),
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
            hidden: false,
            is_dir: false,
            custom: HashMap::new(),
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
    fn every_legacy_mode_produces_a_valid_preview() {
        let entries = [entry("First Draft.txt"), entry("Second Draft.txt")];
        let mut editor = BatchRenameEditor::new(&entries);

        editor.primary = " Draft".to_string();
        editor.secondary.clear();
        assert_eq!(editor.preview()[0].new_name, "First.txt");

        editor.set_mode(BatchRenameMode::Regex);
        editor.primary = "(first|second)".to_string();
        editor.secondary = "final-$1".to_string();
        assert_eq!(editor.preview()[1].new_name, "final-Second Draft.txt");

        editor.set_mode(BatchRenameMode::Number);
        assert_eq!(editor.preview()[0].new_name, "First Draft_001.txt");

        editor.set_mode(BatchRenameMode::Case);
        editor.case_mode = CaseMode::Upper;
        editor.case_target = CaseTarget::Both;
        assert_eq!(editor.preview()[0].new_name, "FIRST DRAFT.TXT");

        editor.set_mode(BatchRenameMode::PrefixSuffix);
        editor.primary = "approved-".to_string();
        editor.secondary = "-final".to_string();
        assert_eq!(
            editor.preview()[0].new_name,
            "approved-First Draft-final.txt"
        );

        editor.set_mode(BatchRenameMode::DateTime);
        editor.date_source = DateSource::Modified;
        editor.position = InsertPosition::Prefix;
        editor.secondary = "_".to_string();
        assert!(editor.preview()[0].new_name.ends_with("_First Draft.txt"));
    }

    #[test]
    fn duplicate_and_invalid_results_are_refused_before_service_submission() {
        let entries = [entry("one.txt"), entry("two.txt")];
        let mut editor = BatchRenameEditor::new(&entries);
        editor.set_mode(BatchRenameMode::Number);
        editor.position = InsertPosition::Replace;
        editor.number_digits = 1;
        editor.number_start = 1;
        assert!(editor.request().is_ok());

        editor.set_mode(BatchRenameMode::PrefixSuffix);
        editor.primary = "CON".to_string();
        editor.secondary.clear();
        editor.sources[0].name = ".txt".to_string();
        assert!(editor.request().is_err());
    }

    #[test]
    fn regex_options_date_tokens_and_single_changed_item_match_legacy_behavior() {
        let entries = [entry("one-one.txt"), entry("two.txt")];
        let mut editor = BatchRenameEditor::new(&entries);
        editor.set_mode(BatchRenameMode::Regex);
        editor.primary = "one".to_string();
        editor.secondary = "x".to_string();
        editor.replace_all = false;
        assert_eq!(editor.preview()[0].new_name, "x-one.txt");
        assert_eq!(editor.request().unwrap().len(), 1);

        editor.replace_all = true;
        assert_eq!(editor.preview()[0].new_name, "x-x.txt");

        editor.set_mode(BatchRenameMode::DateTime);
        editor.date_source = DateSource::Modified;
        editor.position = InsertPosition::Replace;
        editor.primary = "YYYY YY MM M DD D HH H hh h mm m ss s A a".to_string();
        let name = editor.preview()[0].new_name.clone();
        let date: DateTime<Local> = SystemTime::UNIX_EPOCH.into();
        let hour_12 = match date.hour() % 12 {
            0 => 12,
            hour => hour,
        };
        let meridiem = if date.hour() >= 12 { "PM" } else { "AM" };
        let expected = format!(
            "{:04} {:02} {:02} {} {:02} {} {:02} {} {hour_12:02} {hour_12} {:02} {} {:02} {} {meridiem} {}.txt",
            date.year(),
            date.year().rem_euclid(100),
            date.month(),
            date.month(),
            date.day(),
            date.day(),
            date.hour(),
            date.hour(),
            date.minute(),
            date.minute(),
            date.second(),
            date.second(),
            meridiem.to_ascii_lowercase(),
        );
        assert_eq!(name, expected);
    }
}
