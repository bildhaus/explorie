//! `DirectoryWindow` behavior for archives.

use crate::*;

impl DirectoryWindow {
    pub(crate) fn prompt_create_archive(&mut self, cx: &mut Context<Self>) {
        let sources = self.effective_selected_paths();
        self.prompt_create_archive_from_paths(sources, cx);
    }

    pub(crate) fn prompt_create_archive_from_paths(
        &mut self,
        sources: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if sources.is_empty() {
            self.status_message = Some("Select one or more items to archive".to_string());
            cx.notify();
            return;
        }
        if self.mutation.in_progress || self.operations.active_count() > 0 {
            self.status_message =
                Some("Wait for the current filesystem change to finish".to_string());
            cx.notify();
            return;
        }
        let default_name = if sources.len() == 1 {
            sources[0]
                .file_stem()
                .or_else(|| sources[0].file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "archive".to_string())
        } else {
            "archive".to_string()
        };
        self.search.active = false;
        self.mutation.prompt = Some(MutationPrompt::new(
            MutationPromptKind::ArchiveName {
                sources,
                format: ArchiveFormat::Zip,
                compression_level: CompressionLevel::Normal,
            },
            default_name,
        ));
        self.status_message = Some("Configure the native archive".to_string());
        cx.notify();
    }

    pub(crate) fn prompt_extract_archive(&mut self, cx: &mut Context<Self>) {
        let entries = self.effective_selected_entries();
        if entries.len() != 1 || entries[0].is_dir {
            self.status_message = Some("Select exactly one archive file to extract".to_string());
            cx.notify();
            return;
        }
        self.prompt_extract_archive_path(entries[0].path.clone(), cx);
    }

    pub(crate) fn prompt_extract_archive_path(
        &mut self,
        archive_path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if self.mutation.in_progress || self.operations.active_count() > 0 {
            self.status_message =
                Some("Wait for the current filesystem change to finish".to_string());
            cx.notify();
            return;
        }
        let default_name =
            available_folder_name(self.browser.path(), &archive_base_name(&archive_path));
        self.search.active = false;
        self.mutation.prompt = Some(MutationPrompt::new(
            MutationPromptKind::ExtractDirectory { archive_path },
            default_name,
        ));
        self.status_message = Some("Choose a new extraction folder".to_string());
        cx.notify();
    }

    pub(crate) fn inspect_selected_archive(&mut self, cx: &mut Context<Self>) {
        let entries = self.effective_selected_entries();
        if entries.len() != 1 || entries[0].is_dir {
            self.status_message = Some("Select exactly one archive file to inspect".to_string());
            cx.notify();
            return;
        }
        self.inspect_archive(entries[0].path.clone(), cx);
    }

    pub(crate) fn inspect_archive(&mut self, archive_path: PathBuf, cx: &mut Context<Self>) {
        let task = self.services.archives.list(archive_path.clone());
        self.preview.archive_inspection_loading = true;
        self.status_message = Some("Reading archive directory…".to_string());
        self.push_mutation_task(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.preview.archive_inspection_loading = false;
                match result {
                    Ok(info) => {
                        view.status_message = Some(format!(
                            "Archive contains {} entries ({})",
                            info.entry_count,
                            format_size(info.total_size)
                        ));
                        view.preview.archive_inspection = Some((archive_path, info));
                    }
                    Err(error) => {
                        view.status_message = Some(format!("Unable to inspect archive: {error}"));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn close_archive_inspection(&mut self, cx: &mut Context<Self>) {
        self.preview.archive_inspection = None;
        cx.notify();
    }

    pub(crate) fn cycle_archive_format(&mut self, cx: &mut Context<Self>) {
        let Some(MutationPrompt {
            kind:
                MutationPromptKind::ArchiveName {
                    format,
                    compression_level,
                    ..
                },
            ..
        }) = self.mutation.prompt.as_mut()
        else {
            self.status_message = Some("Start archive creation first".to_string());
            cx.notify();
            return;
        };
        *format = match *format {
            ArchiveFormat::Zip => ArchiveFormat::SevenZ,
            ArchiveFormat::SevenZ => ArchiveFormat::TarGz,
            ArchiveFormat::TarGz => ArchiveFormat::Tar,
            ArchiveFormat::Tar | ArchiveFormat::Rar => ArchiveFormat::Zip,
        };
        *compression_level = if *format == ArchiveFormat::Zip {
            CompressionLevel::Normal
        } else {
            CompressionLevel::None
        };
        cx.notify();
    }

    pub(crate) fn cycle_archive_compression(&mut self, cx: &mut Context<Self>) {
        let Some(MutationPrompt {
            kind:
                MutationPromptKind::ArchiveName {
                    format,
                    compression_level,
                    ..
                },
            ..
        }) = self.mutation.prompt.as_mut()
        else {
            self.status_message = Some("Start archive creation first".to_string());
            cx.notify();
            return;
        };
        if *format != ArchiveFormat::Zip {
            self.status_message = Some("Compression levels are available for ZIP".to_string());
            cx.notify();
            return;
        }
        *compression_level = match *compression_level {
            CompressionLevel::None => CompressionLevel::Fast,
            CompressionLevel::Fast => CompressionLevel::Normal,
            CompressionLevel::Normal => CompressionLevel::Best,
            CompressionLevel::Best => CompressionLevel::None,
        };
        cx.notify();
    }

    pub(crate) fn render_archive_inspection(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some((archive_path, info)) = self.preview.archive_inspection.as_ref() else {
            return div().into_any_element();
        };
        let mut rows: Vec<AnyElement> = info
            .entries
            .iter()
            .take(24)
            .enumerate()
            .map(|(index, entry)| {
                div()
                    .id(("archive-entry", index))
                    .flex()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .child(if entry.is_dir { "📁" } else { "📄" })
                    .child(div().flex_1().truncate().child(entry.path.clone()))
                    .child(format_size(entry.size))
                    .into_any_element()
            })
            .collect();
        if info.entries.len() > rows.len() {
            rows.push(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(0x909090))
                    .child(format!(
                        "…and {} more entries",
                        info.entries.len() - rows.len()
                    ))
                    .into_any_element(),
            );
        }
        div()
            .id("archive-inspection")
            .flex()
            .flex_col()
            .max_h(px(240.0))
            .overflow_y_scroll()
            .border_t_1()
            .border_color(self.palette.border)
            .bg(self.palette.panel)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(div().flex_1().truncate().child(format!(
                        "{} • {} • {} entries",
                        archive_path.display(),
                        info.format,
                        info.entry_count
                    )))
                    .child(
                        toolbar_button("close-archive-inspection", "Close", self.palette.control)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.close_archive_inspection(cx)),
                            ),
                    ),
            )
            .children(rows)
            .into_any_element()
    }
}

/// The first of "name", "name 2", "name 3", … that is free in `directory`,
/// as Finder names new folders. Extraction never writes into an existing
/// folder, so the suggestion must not collide.
pub(super) fn available_folder_name(directory: &Path, base: &str) -> String {
    let mut candidate = base.to_string();
    for number in 2..10_000 {
        if std::fs::symlink_metadata(directory.join(&candidate)).is_err() {
            break;
        }
        candidate = format!("{base} {number}");
    }
    candidate
}

/// The prompt error for an extraction folder name that is already taken.
pub(super) fn existing_folder_message(name: &str) -> String {
    format!("A folder named \u{201c}{name}\u{201d} already exists")
}
