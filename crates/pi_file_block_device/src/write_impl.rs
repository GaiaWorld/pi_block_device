//! @impl @api: BlockDeviceWriteBatch::write
//! @design-doc: design/features/03-backend-session.feature
use super::*;

impl BlockDeviceWriteBatch for FileWriteBatch {
    fn write(
        &mut self,
        request: BlockWriteChunk,
    ) -> impl Future<Output = Result<(), DeviceError>> + Send + '_ {
        write(self, request)
    }
    fn prepare(self) -> impl Future<Output = Result<(), DeviceError>> + Send {
        prepare(self)
    }
}

impl Drop for FileWriteBatch {
    fn drop(&mut self) {
        // Only release resources. Never unlink potentially committed data.
        if self.interrupted || self.blocks.values().any(|block| !block.complete()) {
            self.device
                .write_fenced
                .store(true, std::sync::atomic::Ordering::Release);
        }
        let _ = &self.gate;
        #[cfg(test)]
        crate::flush_cancel_tests::retain_cancelled_files(self);
    }
}

// pi_result reports and transfer failures are not std::error::Error. Keep
// the original typed value (including transfer/create evidence), not its text.
#[derive(Debug)]
pub(super) struct IoFailure<T>(pub(super) T);
impl<T: fmt::Display> fmt::Display for IoFailure<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl<T: fmt::Debug + fmt::Display + Send + Sync + 'static> Error for IoFailure<T> {}

fn failure<T: fmt::Debug + fmt::Display + Send + Sync + 'static>(
    operation: DeviceOperation,
    source: T,
) -> DeviceError {
    dev_err(operation, Box::new(IoFailure(source)))
}

pub(super) struct BlockProgress {
    logical_len: u64,
    written: u64,
    padded: bool,
}

impl BlockProgress {
    pub(super) fn complete(&self) -> bool {
        self.written == self.logical_len && self.padded
    }
}

pub(super) fn invalid(operation: DeviceOperation, message: &str) -> DeviceError {
    dev_err(
        operation,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}

pub(super) async fn write(
    batch: &mut FileWriteBatch,
    request: BlockWriteChunk,
) -> Result<(), DeviceError> {
    let operation = DeviceOperation::Write;
    if batch.interrupted {
        return Err(invalid(
            operation,
            "write outcome unresolved; reopen and reconcile required",
        ));
    }
    // 首次 write 才占用 gate；等待取消未发起 I/O，不设置 interrupted。
    // begin 的检查不是准入证据，锁后必须重检其它 batch/恢复产生的围栏。
    if batch.gate.is_none() {
        let gate = batch.device.write_gate.lock_arc().await;
        if batch
            .device
            .write_fenced
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(invalid(
                operation,
                "write outcome unresolved; reopen and reconcile required",
            ));
        }
        batch.gate = Some(gate);
    }
    let block = request.block();
    let logical_len = request.logical_len();
    let (start, physical_len) = if let Some(index) = block.block_index() {
        let size = u64::from(BackendCapabilities::fixed().level_sizes[usize::from(block.level())]);
        if logical_len > size {
            return Err(invalid(operation, "logical length exceeds ordinary slot"));
        }
        (
            u64::from(index)
                .checked_mul(size)
                .ok_or_else(|| invalid(operation, "slot offset overflow"))?,
            size,
        )
    } else {
        (0, logical_len)
    };
    let end = request
        .offset()
        .checked_add(request.chunk().len() as u64)
        .ok_or_else(|| invalid(operation, "chunk end overflow"))?;
    let expected = start
        .checked_add(request.offset())
        .ok_or_else(|| invalid(operation, "physical offset overflow"))?;
    start
        .checked_add(physical_len)
        .ok_or_else(|| invalid(operation, "physical end overflow"))?;
    if end > logical_len {
        return Err(invalid(operation, "chunk exceeds logical length"));
    }
    match batch.blocks.get(&block) {
        Some(progress)
            if progress.logical_len == logical_len
                && progress.written == request.offset()
                && !progress.complete() => {}
        None if request.offset() == 0 => {}
        _ => {
            return Err(invalid(
                operation,
                "noncontiguous, duplicate or inconsistent chunk",
            ));
        }
    }
    let path = batch.device.block_path(block);
    let object = if let Some(version) = block.segment_version() {
        RetiredObject::Segment {
            level: block.level(),
            version,
        }
    } else {
        RetiredObject::LargeBlock {
            id: block
                .large_id()
                .ok_or_else(|| invalid(operation, "invalid large id"))?,
        }
    };
    if batch
        .device
        .retirement_gate
        .lock()
        .map_err(|_| invalid(operation, "retirement gate poisoned"))?
        .contains(&object)
    {
        return Err(invalid(operation, "object deletion already admitted"));
    }
    if !batch.files.contains_key(&path) {
        let exists = batch
            .device
            .namespace
            .try_exists(&path)
            .await
            .map_err(|source| failure(operation, source))?;
        // An absent segment can only materialize its first slot. Never invent
        // missing live slots by zero-filling a physical gap. Large IDs never reuse.
        if (!exists && start != 0) || (exists && block.large_id().is_some()) {
            return Err(invalid(
                operation,
                "missing segment prefix or existing large object",
            ));
        }
        batch.interrupted = true;
        let file = if exists {
            batch
                .device
                .namespace
                .open(&path, FileAccessMode::Append)
                .await
                .map_err(|source| failure(operation, source))?
        } else {
            batch
                .device
                .namespace
                .create_new(&path, FileAccessMode::Append)
                .await
                .map_err(|source| failure(operation, source))?
        };
        batch.files.insert(path.clone(), file);
    }
    let file = batch
        .files
        .get_mut(&path)
        .ok_or_else(|| invalid(operation, "missing append resource"))?;
    let actual = file
        .byte_len()
        .await
        .map_err(|source| failure(operation, source))?;
    if actual != expected {
        return Err(invalid(operation, "physical tail differs from slot offset"));
    }
    batch.blocks.entry(block).or_insert(BlockProgress {
        logical_len,
        written: 0,
        padded: false,
    });
    // Set before polling mutation. Cancellation may leave owned backend I/O in
    // flight; only observed completion clears this internal recovery fence.
    batch.interrupted = true;
    file.append(request.chunk().clone())
        .await
        .map_err(|source| failure(operation, source))?;
    #[cfg(test)]
    crate::write_cancel_tests::pause(&batch.device, false).await;
    if end == logical_len && physical_len > logical_len {
        let padding = usize::try_from(physical_len - logical_len)
            .map_err(|_| invalid(operation, "padding length overflow"))?;
        file.append(Bytes::from(vec![0; padding]))
            .await
            .map_err(|source| failure(operation, source))?;
    }
    let physical_end = start
        .checked_add(if end == logical_len {
            physical_len
        } else {
            end
        })
        .ok_or_else(|| invalid(operation, "physical completion overflow"))?;
    if file
        .byte_len()
        .await
        .map_err(|source| failure(operation, source))?
        != physical_end
    {
        return Err(invalid(
            operation,
            "physical tail changed during append; recovery required",
        ));
    }
    let progress = batch
        .blocks
        .get_mut(&block)
        .ok_or_else(|| invalid(operation, "missing block progress"))?;
    progress.written = end;
    progress.padded = end == logical_len;
    batch.interrupted = false;
    Ok(())
}

pub(super) async fn prepare(mut batch: FileWriteBatch) -> Result<(), DeviceError> {
    let operation = DeviceOperation::Prepare;
    if batch.interrupted || batch.blocks.values().any(|block| !block.complete()) {
        return Err(invalid(operation, "incomplete or unresolved batch"));
    }
    if batch.files.is_empty() {
        return Ok(());
    }
    batch.interrupted = true;
    // Every append (including padding) was awaited on these exact handles.
    // Flush is not a completion barrier for other handles' pending operations.
    for file in batch.files.values_mut() {
        file.flush(FileFlushMode::DataAndMetadata)
            .await
            .map_err(|source| failure(operation, source))?;
    }
    #[cfg(test)]
    crate::write_cancel_tests::pause(&batch.device, true).await;
    batch.interrupted = false;
    Ok(())
}
