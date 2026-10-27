//! @impl @api: BlockDeviceBackend::read
//! @design-doc: design/features/16-architecture-boundaries.feature
use super::*;
use pi_async_fs::{MmapRange, ReadMmapHandle};
use std::task::{Poll, Waker};

// The cache holds completion metadata, never a strong mapping owner.
pub(super) struct MappingEntry {
    owner: std::sync::Weak<MappedOwner>,
    completion: Arc<Mutex<(bool, Option<Waker>)>>,
}

pub(super) struct MappedOwner {
    mapping: Option<ReadMmapHandle>,
    completion: Arc<Mutex<(bool, Option<Waker>)>>,
    #[cfg(test)]
    path: PathBuf,
}

impl MappedOwner {
    pub(super) fn as_bytes(&self) -> &[u8] {
        // The handle is only taken during exclusive destruction.
        self.mapping.as_ref().expect("live mapped owner").as_bytes()
    }
}

impl Drop for MappedOwner {
    fn drop(&mut self) {
        #[cfg(test)]
        crate::mapping_drop_tests::before_unregister(&self.path);
        drop(self.mapping.take());
        // 注销先于完成信号；标准锁仅保护状态，不包围 I/O/await 或唤醒。
        // Drop 不取得 mappings 异步锁，避免与等注销的 read 互相等待。
        let wake = {
            let mut completion = self
                .completion
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            completion.0 = true;
            completion.1.take()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }
}

pub(super) async fn read(
    device: &Arc<FileBlockDevice>,
    lease: &FileSnapshotLease,
    block: BlockId,
) -> Result<FileReadBuf, DeviceError> {
    let (start, len) = if let Some(index) = block.block_index() {
        let len = u64::from(BackendCapabilities::fixed().level_sizes[usize::from(block.level())]);
        (
            u64::from(index)
                .checked_mul(len)
                .ok_or_else(|| invalid("slot offset overflow"))?,
            len,
        )
    } else {
        let id = block
            .large_id()
            .ok_or_else(|| invalid("invalid large id"))?;
        let len = lease
            ._state
            .large_blocks
            .entries
            .get(&id)
            .ok_or_else(|| invalid("large length absent from lease"))?
            .logical_len;
        (0, len)
    };
    if len == 0 || len > isize::MAX as u64 || usize::try_from(len).is_err() {
        return Err(invalid("mapping length exceeds addressable slice"));
    }
    let end = start
        .checked_add(len)
        .ok_or_else(|| invalid("mapping end overflow"))?;
    // 弱缓存不延长映射寿命；异步锁使同一 slot 的并发读只建立一次映射。
    // 不跨 await 持有标准 MutexGuard。依赖负责稳定文件身份和范围准入。
    let mut mappings = device.mappings.lock().await;
    mappings.retain(|_, entry| {
        !entry
            .completion
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .0
    });
    if let Some(mapping) = mappings.get(&block).and_then(|entry| entry.owner.upgrade()) {
        if mapping.as_bytes().len() as u64 != len {
            return Err(invalid("lease length differs from existing mapping"));
        }
        return Ok(FileReadBuf {
            mapping,
            lease: lease.clone(),
        });
    }
    if let Some(entry) = mappings.get(&block) {
        // 外层 Arc 归零不证明内部范围租约已注销。保留完成元数据，并在
        // 同一异步准入锁内等待 Drop 发布。只有一个等待者可访问此 waker；
        // 取消释放准入锁，下一等待者覆盖旧 waker，不影响真实注销。
        std::future::poll_fn(|cx| {
            let mut completion = entry
                .completion
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if completion.0 {
                Poll::Ready(())
            } else {
                completion.1 = Some(cx.waker().clone());
                #[cfg(test)]
                crate::mapping_drop_tests::waiting(&device.block_path(block));
                Poll::Pending
            }
        })
        .await;
        mappings.remove(&block);
    }
    let file = device
        .namespace
        .open(&device.block_path(block), FileAccessMode::ReadMmap)
        .await
        .map_err(|error| failure(error.to_string()))?;
    let actual = file
        .byte_len()
        .await
        .map_err(|error| failure(error.to_string()))?;
    if actual < end || (block.large_id().is_some() && actual != len) {
        return Err(invalid("physical length does not cover frozen block range"));
    }
    let range = MmapRange::new(start, end).map_err(|error| invalid(&error.to_string()))?;
    // SAFETY: unsafe create/open establish the caller's lifetime-long external
    // obligations. All backend I/O uses the dependency's shared registry; mapped
    // bytes are immutable, fixed ranges and this lease protects backend deletion.
    let mapping: ReadMmapHandle = unsafe { file.map_read_only_uncoordinated(range) }
        .await
        .map_err(|error| failure(error.to_string()))?;
    let completion = Arc::new(Mutex::new((false, None)));
    let mapping = Arc::new(MappedOwner {
        mapping: Some(mapping),
        completion: Arc::clone(&completion),
        #[cfg(test)]
        path: device.block_path(block),
    });
    mappings.insert(
        block,
        MappingEntry {
            owner: Arc::downgrade(&mapping),
            completion,
        },
    );
    Ok(FileReadBuf {
        mapping,
        lease: lease.clone(),
    })
}

fn invalid(message: &str) -> DeviceError {
    dev_err(
        DeviceOperation::Read,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}

fn failure(message: String) -> DeviceError {
    dev_err(
        DeviceOperation::Read,
        Box::new(std::io::Error::other(message)),
    )
}
