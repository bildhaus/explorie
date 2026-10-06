//! Background helpers that run undo and redo actions.

use crate::*;

pub(crate) fn finish_awaited_recovery(store: &Option<OperationRecoveryStore>, ids: &[String]) {
    if let Some(store) = store.as_ref() {
        let _ = store.remove(ids);
    }
}

pub(crate) async fn await_file_operation(
    services: NativeServices,
    request: FileOperationRequest,
    recovery_store: Option<OperationRecoveryStore>,
) -> ServiceResult<FileOperationResult> {
    let recovery_ids = recovery_store
        .as_ref()
        .map_or_else(|| Ok(Vec::new()), |store| store.record(&request))
        .map_err(ServiceError::from)?;
    let subscription = services.subscribe_async();
    let job_id = match services.mutations.start_file_operation(request) {
        Ok(job_id) => job_id,
        Err(error) => {
            if let Some(store) = recovery_store.as_ref() {
                let _ = store.remove(&recovery_ids);
            }
            return Err(error);
        }
    };
    loop {
        let ServiceEvent::FileOperation(event) = subscription.next().await else {
            continue;
        };
        if event.job_id != job_id {
            continue;
        }
        match event.state {
            explorie_native_services::FileOperationState::Running => {}
            explorie_native_services::FileOperationState::Completed => {
                finish_awaited_recovery(&recovery_store, &recovery_ids);
                return event.result.ok_or_else(|| {
                    ServiceError::new(
                        ErrorCode::Internal,
                        "File operation completed without a result",
                    )
                });
            }
            explorie_native_services::FileOperationState::Cancelled => {
                finish_awaited_recovery(&recovery_store, &recovery_ids);
                return Err(event.error.unwrap_or_else(|| {
                    ServiceError::new(ErrorCode::Cancelled, "File operation cancelled")
                }));
            }
            explorie_native_services::FileOperationState::Failed => {
                finish_awaited_recovery(&recovery_store, &recovery_ids);
                return Err(event.error.unwrap_or_else(|| {
                    ServiceError::new(ErrorCode::Internal, "File operation failed")
                }));
            }
        }
    }
}

#[cfg(test)]
pub(crate) async fn undo_action(
    action: UndoAction,
    services: NativeServices,
) -> ServiceResult<UndoAction> {
    undo_action_with_progress(
        action,
        services,
        None,
        Arc::new(AtomicBool::new(false)),
        |_| {},
    )
    .await
}

pub(crate) fn undo_action_item_count(action: &UndoAction) -> usize {
    match action {
        UndoAction::Copy { targets, .. } => targets.len(),
        UndoAction::Move { pairs, .. } | UndoAction::BatchRename { pairs } => pairs.len(),
        UndoAction::Create { .. } | UndoAction::Rename { .. } => 1,
    }
}

pub(crate) fn undo_cancelled_error() -> ServiceError {
    ServiceError::new(ErrorCode::Cancelled, "Undo cancelled")
}

pub(crate) async fn await_undo_file_operation(
    services: NativeServices,
    request: FileOperationRequest,
    recovery_store: Option<OperationRecoveryStore>,
    cancellation: &AtomicBool,
    on_update: &mut impl FnMut(UndoProgressUpdate),
) -> ServiceResult<FileOperationResult> {
    if cancellation.load(Ordering::Acquire) {
        return Err(undo_cancelled_error());
    }
    let recovery_ids = recovery_store
        .as_ref()
        .map_or_else(|| Ok(Vec::new()), |store| store.record(&request))
        .map_err(ServiceError::from)?;
    let subscription = services.subscribe_async();
    let job_id = match services.mutations.start_file_operation(request) {
        Ok(job_id) => job_id,
        Err(error) => {
            if let Some(store) = recovery_store.as_ref() {
                let _ = store.remove(&recovery_ids);
            }
            return Err(error);
        }
    };
    on_update(UndoProgressUpdate::Started(job_id.clone()));
    if cancellation.load(Ordering::Acquire) {
        services.mutations.cancel_file_operation(&job_id);
    }
    loop {
        let ServiceEvent::FileOperation(event) = subscription.next().await else {
            continue;
        };
        if event.job_id != job_id {
            continue;
        }
        match event.state {
            explorie_native_services::FileOperationState::Running => {
                if let Some(progress) = event.progress {
                    on_update(UndoProgressUpdate::Progress(progress));
                }
                if cancellation.load(Ordering::Acquire) {
                    services.mutations.cancel_file_operation(&job_id);
                }
            }
            explorie_native_services::FileOperationState::Completed => {
                finish_awaited_recovery(&recovery_store, &recovery_ids);
                return event.result.ok_or_else(|| {
                    ServiceError::new(
                        ErrorCode::Internal,
                        "Undo file operation completed without a result",
                    )
                });
            }
            explorie_native_services::FileOperationState::Cancelled => {
                finish_awaited_recovery(&recovery_store, &recovery_ids);
                return Err(event.error.unwrap_or_else(undo_cancelled_error));
            }
            explorie_native_services::FileOperationState::Failed => {
                finish_awaited_recovery(&recovery_store, &recovery_ids);
                return Err(event.error.unwrap_or_else(|| {
                    ServiceError::new(ErrorCode::Internal, "Undo file operation failed")
                }));
            }
        }
    }
}

pub(crate) async fn undo_action_with_progress(
    action: UndoAction,
    services: NativeServices,
    recovery_store: Option<OperationRecoveryStore>,
    cancellation: Arc<AtomicBool>,
    mut on_update: impl FnMut(UndoProgressUpdate),
) -> ServiceResult<UndoAction> {
    match action {
        UndoAction::Copy {
            request,
            targets,
            snapshots,
        } => {
            if targets.len() != snapshots.len() {
                return Err(ServiceError::new(
                    ErrorCode::Internal,
                    "Copy undo is missing its safety snapshot",
                ));
            }
            for (target, snapshot) in targets.iter().zip(&snapshots) {
                if cancellation.load(Ordering::Acquire) {
                    return Err(undo_cancelled_error());
                }
                if !services.mutations.path_exists(target.clone()).await? {
                    on_update(UndoProgressUpdate::ItemCompleted);
                    continue;
                }
                if !services
                    .mutations
                    .path_matches_snapshot(target.clone(), snapshot.clone())
                    .await?
                {
                    return Err(ServiceError::new(
                        ErrorCode::Conflict,
                        format!(
                            "{} changed after it was copied; refusing to move it to Trash",
                            target.display()
                        ),
                    ));
                }
                await_undo_file_operation(
                    services.clone(),
                    FileOperationRequest {
                        kind: FileOperationKind::Trash,
                        sources: vec![target.clone()],
                        destination: None,
                        conflict_policy: ConflictPolicy::Error,
                    },
                    recovery_store.clone(),
                    &cancellation,
                    &mut on_update,
                )
                .await?;
                on_update(UndoProgressUpdate::ItemCompleted);
            }
            Ok(UndoAction::Copy {
                request,
                targets,
                snapshots,
            })
        }
        UndoAction::Move { mut request, pairs } => {
            let mut restored_pairs = Vec::with_capacity(pairs.len());
            for (source, target) in pairs {
                if cancellation.load(Ordering::Acquire) {
                    return Err(undo_cancelled_error());
                }
                if !services.mutations.path_exists(target.clone()).await? {
                    if services.mutations.path_exists(source.clone()).await? {
                        on_update(UndoProgressUpdate::ItemCompleted);
                        restored_pairs.push((source, target));
                        continue;
                    }
                    return Err(ServiceError::new(
                        ErrorCode::NotFound,
                        format!(
                            "Neither moved path nor original path exists: {}",
                            target.display()
                        ),
                    ));
                }
                let parent = source.parent().map(ToOwned::to_owned).ok_or_else(|| {
                    ServiceError::new(
                        ErrorCode::InvalidInput,
                        format!("Original path has no parent: {}", source.display()),
                    )
                })?;
                let result = await_undo_file_operation(
                    services.clone(),
                    FileOperationRequest {
                        kind: FileOperationKind::Move,
                        sources: vec![target.clone()],
                        destination: Some(parent),
                        conflict_policy: ConflictPolicy::Error,
                    },
                    recovery_store.clone(),
                    &cancellation,
                    &mut on_update,
                )
                .await?;
                let restored = result.targets.into_iter().next().unwrap_or(source.clone());
                let restored = restore_original_name(&services, restored, &source).await;
                on_update(UndoProgressUpdate::ItemCompleted);
                restored_pairs.push((restored, target));
            }
            // Redo must move the items from where undo actually put them.
            request.sources = restored_pairs
                .iter()
                .map(|(restored, _)| restored.clone())
                .collect();
            Ok(UndoAction::Move {
                request,
                pairs: restored_pairs,
            })
        }
        UndoAction::Create { kind, path } => {
            if cancellation.load(Ordering::Acquire) {
                return Err(undo_cancelled_error());
            }
            if services.mutations.path_exists(path.clone()).await? {
                await_undo_file_operation(
                    services.clone(),
                    FileOperationRequest {
                        kind: FileOperationKind::Trash,
                        sources: vec![path.clone()],
                        destination: None,
                        conflict_policy: ConflictPolicy::Error,
                    },
                    recovery_store.clone(),
                    &cancellation,
                    &mut on_update,
                )
                .await?;
            }
            on_update(UndoProgressUpdate::ItemCompleted);
            Ok(UndoAction::Create { kind, path })
        }
        UndoAction::Rename { before, after } => {
            if cancellation.load(Ordering::Acquire) {
                return Err(undo_cancelled_error());
            }
            if !services.mutations.path_exists(after.clone()).await? {
                if services.mutations.path_exists(before.clone()).await? {
                    on_update(UndoProgressUpdate::ItemCompleted);
                    return Ok(UndoAction::Rename { before, after });
                }
                return Err(ServiceError::new(
                    ErrorCode::NotFound,
                    format!("Renamed path no longer exists: {}", after.display()),
                ));
            }
            let old_name = path_base_name(&before)?;
            let restored = services
                .mutations
                .rename_path(after.clone(), old_name)
                .await?;
            on_update(UndoProgressUpdate::ItemCompleted);
            Ok(UndoAction::Rename {
                before: PathBuf::from(restored),
                after,
            })
        }
        UndoAction::BatchRename { pairs } => {
            if cancellation.load(Ordering::Acquire) {
                return Err(undo_cancelled_error());
            }
            let request = pairs
                .iter()
                .rev()
                .map(|(before, after)| {
                    Ok(BatchRenameItem {
                        source_path: after.clone(),
                        new_base_name: path_base_name(before)?,
                    })
                })
                .collect::<ServiceResult<Vec<_>>>()?;
            services.mutations.batch_rename(request).await?;
            for _ in &pairs {
                on_update(UndoProgressUpdate::ItemCompleted);
            }
            Ok(UndoAction::BatchRename { pairs })
        }
    }
}

/// A "Keep both" move committed under a suffixed name such as `foo (1)`, and
/// moving it back keeps that name. Put the original name back when it is still
/// free; otherwise leave the item where the move-back placed it.
async fn restore_original_name(
    services: &NativeServices,
    restored: PathBuf,
    original: &Path,
) -> PathBuf {
    if restored == original || restored.parent() != original.parent() {
        return restored;
    }
    let from = restored.clone();
    let to = original.to_path_buf();
    match services
        .context
        .spawn_blocking(move || {
            explorie_core::rename_noreplace(&from, &to).map_err(ServiceError::from)
        })
        .await
    {
        Ok(()) => original.to_path_buf(),
        Err(_) => restored,
    }
}

pub(crate) async fn redo_action(
    action: UndoAction,
    services: NativeServices,
    recovery_store: Option<OperationRecoveryStore>,
) -> ServiceResult<UndoAction> {
    match action {
        UndoAction::Copy { request, .. } => {
            let result =
                await_file_operation(services, request.clone(), recovery_store.clone()).await?;
            Ok(UndoAction::Copy {
                request,
                targets: result.targets,
                snapshots: result.target_snapshots,
            })
        }
        UndoAction::Move { request, .. } => {
            let result =
                await_file_operation(services, request.clone(), recovery_store.clone()).await?;
            if result.targets.len() != request.sources.len() {
                return Err(ServiceError::new(
                    ErrorCode::Internal,
                    "Redone move returned an incomplete target list",
                ));
            }
            let pairs = request
                .sources
                .iter()
                .cloned()
                .zip(result.targets)
                .collect();
            Ok(UndoAction::Move { request, pairs })
        }
        UndoAction::Create { kind, path } => {
            let parent = path.parent().map(ToOwned::to_owned).ok_or_else(|| {
                ServiceError::new(
                    ErrorCode::InvalidInput,
                    format!("Created path has no parent: {}", path.display()),
                )
            })?;
            let name = path_base_name(&path)?;
            let recreated = match &kind {
                CreatedKind::Folder => services.mutations.create_folder(parent, name).await?,
                CreatedKind::Note => services.mutations.create_note(parent, name).await?,
                CreatedKind::WebsiteLink { url } => {
                    services
                        .mutations
                        .create_website_link(parent, name, url.clone())
                        .await?
                }
            };
            Ok(UndoAction::Create {
                kind,
                path: PathBuf::from(recreated),
            })
        }
        UndoAction::Rename { before, after } => {
            if !services.mutations.path_exists(before.clone()).await? {
                if services.mutations.path_exists(after.clone()).await? {
                    return Ok(UndoAction::Rename { before, after });
                }
                return Err(ServiceError::new(
                    ErrorCode::NotFound,
                    format!(
                        "Original rename path no longer exists: {}",
                        before.display()
                    ),
                ));
            }
            let new_name = path_base_name(&after)?;
            let renamed = services
                .mutations
                .rename_path(before.clone(), new_name)
                .await?;
            Ok(UndoAction::Rename {
                before,
                after: PathBuf::from(renamed),
            })
        }
        UndoAction::BatchRename { pairs } => {
            let request = pairs
                .iter()
                .map(|(before, after)| {
                    Ok(BatchRenameItem {
                        source_path: before.clone(),
                        new_base_name: path_base_name(after)?,
                    })
                })
                .collect::<ServiceResult<Vec<_>>>()?;
            services.mutations.batch_rename(request).await?;
            Ok(UndoAction::BatchRename { pairs })
        }
    }
}
