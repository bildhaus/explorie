use std::path::PathBuf;

use explorie_native_services::{ArchiveFormat, CompressionLevel};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MutationPromptKind {
    NewFolder,
    NewNote,
    NewWebsiteLinkName,
    NewWebsiteLinkUrl {
        name: String,
    },
    Rename {
        source: PathBuf,
    },
    PermanentDelete {
        items: Vec<(PathBuf, bool)>,
    },
    Trash {
        paths: Vec<PathBuf>,
    },
    ArchiveName {
        sources: Vec<PathBuf>,
        format: ArchiveFormat,
        compression_level: CompressionLevel,
    },
    ArchivePassword {
        sources: Vec<PathBuf>,
        output_path: PathBuf,
        format: ArchiveFormat,
        compression_level: CompressionLevel,
    },
    ExtractDirectory {
        archive_path: PathBuf,
    },
    ExtractPassword {
        archive_path: PathBuf,
        output_dir: PathBuf,
        allow_extended_limits: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationPrompt {
    pub kind: MutationPromptKind,
    pub input: String,
    pub replace_on_type: bool,
    pub error: Option<String>,
}

impl MutationPrompt {
    pub fn new(kind: MutationPromptKind, input: String) -> Self {
        Self {
            replace_on_type: matches!(kind, MutationPromptKind::Rename { .. }),
            kind,
            input,
            error: None,
        }
    }

    pub fn title(&self) -> &'static str {
        match self.kind {
            MutationPromptKind::NewFolder => "New Folder",
            MutationPromptKind::NewNote => "New Markdown Note",
            MutationPromptKind::NewWebsiteLinkName
            | MutationPromptKind::NewWebsiteLinkUrl { .. } => "New Website Link",
            MutationPromptKind::Rename { .. } => "Rename Item",
            MutationPromptKind::PermanentDelete { .. } => "Delete Permanently",
            MutationPromptKind::Trash { .. } => "Delete Items",
            MutationPromptKind::ArchiveName { .. } | MutationPromptKind::ArchivePassword { .. } => {
                "Create Archive"
            }
            MutationPromptKind::ExtractDirectory { .. }
            | MutationPromptKind::ExtractPassword { .. } => "Extract Archive",
        }
    }

    pub fn submit_hint(&self) -> &'static str {
        match self.kind {
            MutationPromptKind::PermanentDelete { .. } => {
                "Type DELETE exactly, then press Enter • Esc to cancel"
            }
            MutationPromptKind::Trash { .. } => "Enter to move to Trash • Esc to cancel",
            MutationPromptKind::ArchiveName { .. } => {
                "Choose format and compression above • Enter to continue • Esc to cancel"
            }
            MutationPromptKind::ExtractPassword {
                allow_extended_limits: true,
                ..
            } => "Enter to approve extended extraction limits • Esc to cancel",
            MutationPromptKind::ArchivePassword { .. }
            | MutationPromptKind::ExtractPassword { .. } => {
                "Enter for no password or type one first • Esc to cancel"
            }
            _ => "Enter to apply • Esc to cancel",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_titles_describe_the_native_mutation() {
        assert_eq!(
            MutationPrompt::new(MutationPromptKind::NewFolder, String::new()).title(),
            "New Folder"
        );
        assert_eq!(
            MutationPrompt::new(
                MutationPromptKind::Rename {
                    source: PathBuf::from("old.txt")
                },
                "old.txt".into()
            )
            .title(),
            "Rename Item"
        );
        let prompt = MutationPrompt::new(
            MutationPromptKind::PermanentDelete {
                items: vec![(PathBuf::from("old.txt"), false)],
            },
            String::new(),
        );
        assert_eq!(prompt.title(), "Delete Permanently");
        assert!(prompt.submit_hint().contains("DELETE exactly"));
        let trash = MutationPrompt::new(
            MutationPromptKind::Trash {
                paths: vec![PathBuf::from("old.txt")],
            },
            String::new(),
        );
        assert_eq!(trash.title(), "Delete Items");
        assert!(trash.submit_hint().contains("Enter"));
        let password = MutationPrompt::new(
            MutationPromptKind::ExtractPassword {
                archive_path: PathBuf::from("bundle.zip"),
                output_dir: PathBuf::from("output"),
                allow_extended_limits: false,
            },
            "secret".to_string(),
        );
        assert_eq!(password.input, "secret");
        assert!(matches!(
            password.kind,
            MutationPromptKind::ExtractPassword { .. }
        ));
    }
}
