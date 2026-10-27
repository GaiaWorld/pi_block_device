use super::*;

pub(super) fn create(
    config: FileBlockDeviceConfig,
) -> impl Future<Output = Result<Arc<FileBlockDevice>, FileDeviceError>> + Send + 'static {
    async move {
        validate_root(&config.root)?;
        let namespace = LocalFileNamespace::new();
        if pi_exists(&namespace, &config.root).await? {
            return Err(FileDeviceError::AlreadyExists);
        }
        namespace
            .create_dir(&config.root)
            .await
            .map_err(|failure| create_error::wrap("create_dir", failure))?;
        #[cfg(test)]
        create_tests::after_root_created(&config.root);
        let path = config.root.join(".manifest.staging");
        let mut owns_staging = false;
        let result = async {
            let device_id = BlockDeviceId::generate().map_err(io_other)?;
            let encoded = encode_manifest(Manifest { device_id });
            let mut file: LocalFile = namespace
                .create_new(&path, FileAccessMode::Append)
                .await
                .map_err(|failure| {
                    owns_staging = matches!(
                        failure.target_evidence(),
                        CreateTargetEvidence::CreatedByOperation
                    );
                    create_error::wrap("create_new staging", failure)
                })?;
            owns_staging = true;
            file.append(Bytes::copy_from_slice(&encoded))
                .await
                .map_err(|error| {
                    io_other(format!("pi_async_fs append manifest failed: {error}"))
                })?;
            #[cfg(test)]
            manifest_publish_tests::before_flush(&config.root).await;
            file.flush(FileFlushMode::DataAndMetadata)
                .await
                .map_err(|error| io_other(format!("pi_async_fs flush manifest failed: {error}")))?;
            drop(file);
            #[cfg(test)]
            create_tests::before_manifest_publish(&config.root);
            namespace
                .rename(&path, &manifest_path(&config.root))
                .await
                .map_err(|failure| {
                    // Unknown/committed publication must not authorize path cleanup.
                    owns_staging = matches!(
                        failure.commit_evidence(),
                        pi_async_fs::RenameCommitEvidence::NotRenamedByOperation
                    );
                    create_error::wrap_rename(failure)
                })?;
            Ok::<BlockDeviceId, FileDeviceError>(device_id)
        }
        .await;
        let device_id = match result {
            Ok(device_id) => device_id,
            Err(error) => {
                // The file scope has ended before namespace cleanup. Evidence is
                // historical ownership, not protection against path replacement.
                if owns_staging {
                    let _ = namespace.remove_file(&path).await;
                }
                // create_dir succeeded; never recurse into external contents.
                let _ = namespace.remove_dir(&config.root).await;
                return Err(error);
            }
        };
        Ok(Arc::new(FileBlockDevice {
            root: config.root,
            device_id,
            namespace,
            retirements: Arc::new(Mutex::new(VecDeque::new())),
            snapshot_states: Arc::new(Mutex::new(Vec::new())),
            retirement_gate: Arc::new(Mutex::new(Vec::new())),
            reclaim_gate: async_lock::Mutex::new(()),
            unresolved_removals: Mutex::new(Vec::new()),
            mappings: async_lock::Mutex::new(std::collections::BTreeMap::new()),
            write_gate: Arc::new(async_lock::Mutex::new(())),
            write_fenced: std::sync::atomic::AtomicBool::new(false),
            recovering: std::sync::atomic::AtomicBool::new(false),
        }))
    }
}
