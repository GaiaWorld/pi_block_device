//! 文件块设备：固定 manifest 与已提交块读取。
//!
//! 写入批次完成严格追加、完整性检查和文件 sync；不承诺目录项突然掉电保证。
//! 所有文件 I/O 走锁定的 pi_async_fs = 0.1.2。

//! 后端 trait 与批准设计将 create/open、write 和 prepare 固定为
//! `impl Future<...> + Send` 返回形态（含 `self: &Arc<Self>` 任意接收者），
//! 因此刻意保留返回位置 impl Trait 写法而不改写为 async fn。
#![allow(clippy::manual_async_fn)]

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_lite::StreamExt;
use pi_async_fs::{
    CreateTargetEvidence, FileAccessMode, FileFlushMode, FileIo, FileNamespace, LocalFile,
    LocalFileNamespace, ReadTargetRegion,
};
use pi_block_device::{
    BackendCapabilities, BlockDeviceBackend, BlockDeviceId, BlockDeviceState,
    BlockDeviceWriteBatch, BlockId, BlockWriteChunk, BoxError, DeviceError, DeviceOperation,
    InspectReport, ProtectedReadBuf, ReconcileError, RetiredObject, RetirementReport,
};

const MANIFEST_NAME: &str = "manifest.bin";
const MAGIC: [u8; 8] = *b"PIFBLK01";
const FORMAT_VERSION: u16 = 1;
const MANIFEST_LEN: usize = 8 + 2 + 2 + 16 + 4 + 15 * 4 + 1;

mod create;
mod create_error;
#[cfg(test)]
mod create_tests;
#[cfg(test)]
mod flush_cancel_tests;
#[cfg(test)]
mod manifest_publish_tests;
#[cfg(test)]
mod mapping_drop_tests;
mod read_impl;
mod reclaim_impl;
mod reconcile_retire;
#[cfg(test)]
mod retirement_tests;
#[cfg(test)]
mod write_cancel_tests;
mod write_impl;

/// 文件后端配置；固定协议事实不能由调用方配置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileBlockDeviceConfig {
    /// 后端根目录（绝对路径）。
    pub root: PathBuf,
}

/// 文件后端错误冻结六类（不含额外隐式能力分支）。
#[derive(Debug)]
pub enum FileDeviceError {
    /// root 为空或相对路径等非法配置。
    InvalidConfiguration,
    /// create 的目标已存在。
    AlreadyExists,
    /// open 的目标或 manifest 不存在。
    NotFound,
    /// manifest 格式或固定协议事实非法。
    InvalidManifest,
    /// manifest 身份与调用方期望不符。
    IdentityMismatch,
    /// 其它文件系统错误。
    Io { source: BoxError },
}

impl fmt::Display for FileDeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration => f.write_str("invalid configuration"),
            Self::AlreadyExists => f.write_str("already exists"),
            Self::NotFound => f.write_str("not found"),
            Self::InvalidManifest => f.write_str("invalid manifest"),
            Self::IdentityMismatch => f.write_str("identity mismatch"),
            Self::Io { source } => write!(f, "I/O error: {source}"),
        }
    }
}
impl Error for FileDeviceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source } => Some(source.as_ref()),
            _ => None,
        }
    }
}

/// 第一切片明确未实现的结构化错误来源（不进入公开错误枚举）。
#[derive(Clone, Copy, Debug)]
struct Unimplemented(&'static str);
impl fmt::Display for Unimplemented {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "first-slice unimplemented: {}", self.0)
    }
}
impl Error for Unimplemented {}

fn io_other(message: impl fmt::Display) -> FileDeviceError {
    FileDeviceError::Io {
        source: Box::new(std::io::Error::other(message.to_string())),
    }
}
fn dev_err(operation: DeviceOperation, source: BoxError) -> DeviceError {
    DeviceError {
        operation,
        device_key: None,
        source,
    }
}
/// 固定格式 manifest 的已解码事实。
#[derive(Clone, Copy)]
struct Manifest {
    device_id: BlockDeviceId,
}

/// 文件块设备后端。
pub struct FileBlockDevice {
    root: PathBuf,
    device_id: BlockDeviceId,
    namespace: LocalFileNamespace,
    retirements: Arc<Mutex<VecDeque<RetiredObject>>>,
    snapshot_states: Arc<Mutex<Vec<std::sync::Weak<BlockDeviceState>>>>,
    retirement_gate: Arc<Mutex<Vec<RetiredObject>>>,
    reclaim_gate: async_lock::Mutex<()>,
    unresolved_removals: Mutex<Vec<RetiredObject>>,
    mappings: async_lock::Mutex<std::collections::BTreeMap<BlockId, read_impl::MappingEntry>>,
    write_gate: Arc<async_lock::Mutex<()>>,
    write_fenced: std::sync::atomic::AtomicBool,
    recovering: std::sync::atomic::AtomicBool,
}

/// 与快照租约关联的已读缓冲区。
#[doc(hidden)]
pub struct FileReadBuf {
    mapping: Arc<read_impl::MappedOwner>,
    lease: FileSnapshotLease,
}

/// 第一切片快照租约标记。
#[doc(hidden)]
#[derive(Clone)]
pub struct FileSnapshotLease {
    _state: Arc<BlockDeviceState>,
}

/// 自设备写入批次；Drop 仅释放资源。
#[doc(hidden)]
pub struct FileWriteBatch {
    device: Arc<FileBlockDevice>,
    gate: Option<async_lock::MutexGuardArc<()>>,
    blocks: std::collections::BTreeMap<BlockId, write_impl::BlockProgress>,
    files: std::collections::BTreeMap<PathBuf, LocalFile>,
    interrupted: bool,
}

fn validate_root(root: &Path) -> Result<(), FileDeviceError> {
    if root.as_os_str().is_empty() || !root.is_absolute() {
        return Err(FileDeviceError::InvalidConfiguration);
    }
    Ok(())
}

async fn pi_exists(
    namespace: &LocalFileNamespace,
    path: &PathBuf,
) -> Result<bool, FileDeviceError> {
    namespace
        .try_exists(path)
        .await
        .map_err(|error| io_other(format!("pi_async_fs try_exists failed: {error}")))
}

async fn read_whole(
    namespace: &LocalFileNamespace,
    path: &PathBuf,
) -> Result<Vec<u8>, FileDeviceError> {
    let file: LocalFile = namespace
        .open(path, FileAccessMode::Read)
        .await
        .map_err(|error| io_other(format!("pi_async_fs open-read failed: {error}")))?;
    let length = file
        .byte_len()
        .await
        .map_err(|error| io_other(format!("pi_async_fs byte_len failed: {error}")))?;
    if length > 1 << 20 {
        return Err(FileDeviceError::InvalidManifest);
    }
    let mut buffer = vec![0_u8; length as usize];
    file.read_exact_at(0, &mut buffer, ReadTargetRegion::full())
        .await
        .map_err(|error| io_other(format!("pi_async_fs read_exact_at failed: {error}")))?;
    Ok(buffer)
}

fn manifest_path(root: &Path) -> PathBuf {
    root.join(MANIFEST_NAME)
}

fn encode_manifest(manifest: Manifest) -> [u8; MANIFEST_LEN] {
    let c = BackendCapabilities::fixed();
    let mut out = [0_u8; MANIFEST_LEN];
    let mut p = 0;
    out[p..p + 8].copy_from_slice(&MAGIC);
    p += 8;
    out[p..p + 2].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    p += 2;
    out[p..p + 2].copy_from_slice(&c.protocol_version.to_le_bytes());
    p += 2;
    out[p..p + 16].copy_from_slice(manifest.device_id.as_bytes());
    p += 16;
    out[p..p + 4].copy_from_slice(&c.base_block_size.to_le_bytes());
    p += 4;
    for size in c.level_sizes {
        out[p..p + 4].copy_from_slice(&size.to_le_bytes());
        p += 4;
    }
    out[p] = u8::from(c.independent_large_blocks);
    out
}

fn decode_manifest(bytes: &[u8]) -> Result<Manifest, FileDeviceError> {
    if bytes.len() != MANIFEST_LEN
        || bytes[..8] != MAGIC
        || u16::from_le_bytes([bytes[8], bytes[9]]) != FORMAT_VERSION
    {
        return Err(FileDeviceError::InvalidManifest);
    }
    let c = BackendCapabilities::fixed();
    if u16::from_le_bytes([bytes[10], bytes[11]]) != c.protocol_version
        || u32::from_le_bytes(
            bytes[28..32]
                .try_into()
                .map_err(|_| FileDeviceError::InvalidManifest)?,
        ) != c.base_block_size
        || bytes[MANIFEST_LEN - 1] != 1
    {
        return Err(FileDeviceError::InvalidManifest);
    }
    for (index, expected) in c.level_sizes.iter().enumerate() {
        let start = 32 + index * 4;
        if u32::from_le_bytes(
            bytes[start..start + 4]
                .try_into()
                .map_err(|_| FileDeviceError::InvalidManifest)?,
        ) != *expected
        {
            return Err(FileDeviceError::InvalidManifest);
        }
    }
    let mut id = [0_u8; 16];
    id.copy_from_slice(&bytes[12..28]);
    Ok(Manifest {
        device_id: BlockDeviceId::from_bytes(id).map_err(|_| FileDeviceError::InvalidManifest)?,
    })
}

impl FileBlockDevice {
    /// 完整 manifest 经 flush 后原子非覆盖发布；不保证目录项抗掉电持久性。
    ///
    /// # Safety
    /// From the first poll until the device, all operations and mapped ReadBuf
    /// values are released, uncoordinated participants must not truncate, overwrite,
    /// replace, recreate, delete, map overlapping ranges, or invalidate storage
    /// or permissions of device files. External append may only extend the frozen
    /// tail without changing existing bytes. Backend-managed I/O remains coordinated.
    pub unsafe fn create(
        config: FileBlockDeviceConfig,
    ) -> impl Future<Output = Result<Arc<Self>, FileDeviceError>> + Send + 'static {
        create::create(config)
    }

    /// 非破坏打开并校验固定 manifest；expected_device_id 可选身份断言。
    ///
    /// # Safety
    /// The caller must uphold the same lifetime-long uncoordinated-access
    /// obligations as `Self::create`, including after this device is dropped
    /// while an operation or mapped ReadBuf still exists.
    pub unsafe fn open(
        config: FileBlockDeviceConfig,
        expected_device_id: Option<BlockDeviceId>,
    ) -> impl Future<Output = Result<Arc<Self>, FileDeviceError>> + Send + 'static {
        async move {
            validate_root(&config.root)?;
            let namespace = LocalFileNamespace::new();
            if !pi_exists(&namespace, &config.root).await? {
                return Err(FileDeviceError::NotFound);
            }
            let path = manifest_path(&config.root);
            if !pi_exists(&namespace, &path).await? {
                return Err(FileDeviceError::NotFound);
            }
            let bytes = read_whole(&namespace, &path).await?;
            let manifest = decode_manifest(&bytes)?;
            if expected_device_id.is_some_and(|id| id != manifest.device_id) {
                return Err(FileDeviceError::IdentityMismatch);
            }
            Ok(Arc::new(Self {
                root: config.root,
                device_id: manifest.device_id,
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

    fn block_path(&self, block: BlockId) -> PathBuf {
        if let Some(version) = block.segment_version() {
            self.root.join(format!(
                "segment-{:02x}-{:08x}.bin",
                block.level(),
                version.raw()
            ))
        } else {
            self.root.join(format!(
                "large-{:015x}.bin",
                block.large_block_id().unwrap_or(0)
            ))
        }
    }
}

impl AsRef<[u8]> for FileReadBuf {
    fn as_ref(&self) -> &[u8] {
        self.mapping.as_bytes()
    }
}
impl ProtectedReadBuf for FileReadBuf {
    type Lease = FileSnapshotLease;
    fn lease(&self) -> &Self::Lease {
        &self.lease
    }
}

impl BlockDeviceBackend for FileBlockDevice {
    type SnapshotLease = FileSnapshotLease;
    type ReadBuf = FileReadBuf;
    type WriteBatch = FileWriteBatch;

    fn device_id(&self) -> BlockDeviceId {
        self.device_id
    }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::fixed()
    }
    fn inspect(&self) -> impl Future<Output = Result<InspectReport, DeviceError>> + Send + '_ {
        async move {
            let mut stream = self
                .namespace
                .read_dir(self.root.clone())
                .await
                .map_err(|error| {
                    dev_err(
                        DeviceOperation::Inspect,
                        Box::new(std::io::Error::other(format!(
                            "pi_async_fs read_dir failed: {error}"
                        ))),
                    )
                })?;
            let mut count = 0_u64;
            while let Some(item) = stream.next().await {
                let entry = item.map_err(|error| {
                    dev_err(
                        DeviceOperation::Inspect,
                        Box::new(std::io::Error::other(format!(
                            "pi_async_fs directory entry failed: {error}"
                        ))),
                    )
                })?;
                // The validated identity manifest is not allocated block data.
                if entry.locator != manifest_path(&self.root) {
                    count += 1;
                }
            }
            Ok(InspectReport {
                empty: count == 0,
                observed_objects: count,
            })
        }
    }
    fn begin_write_batch(self: &Arc<Self>) -> Result<Self::WriteBatch, DeviceError> {
        if self.write_fenced.load(std::sync::atomic::Ordering::Acquire) {
            return Err(write_impl::invalid(
                DeviceOperation::BeginWriteBatch,
                "write outcome unresolved; reopen and reconcile required",
            ));
        }
        Ok(FileWriteBatch {
            device: Arc::clone(self),
            gate: None,
            blocks: std::collections::BTreeMap::new(),
            files: std::collections::BTreeMap::new(),
            interrupted: false,
        })
    }
    fn acquire_snapshot_lease(
        self: &Arc<Self>,
        state: Arc<BlockDeviceState>,
    ) -> Result<Self::SnapshotLease, DeviceError> {
        let gate = self.retirement_gate.lock().map_err(|_| {
            dev_err(
                DeviceOperation::AcquireSnapshotLease,
                Box::new(std::io::Error::other("retirement gate poisoned")),
            )
        })?;
        if self.recovering.load(std::sync::atomic::Ordering::Acquire) {
            return Err(write_impl::invalid(
                DeviceOperation::AcquireSnapshotLease,
                "open recovery is in progress",
            ));
        }
        if gate
            .iter()
            .any(|object| reclaim_impl::state_references(&state, *object))
        {
            return Err(dev_err(
                DeviceOperation::AcquireSnapshotLease,
                Box::new(std::io::Error::other("object deletion has been admitted")),
            ));
        }
        self.snapshot_states
            .lock()
            .map_err(|_| {
                dev_err(
                    DeviceOperation::AcquireSnapshotLease,
                    Box::new(Unimplemented("snapshot lease registry poisoned")),
                )
            })?
            .push(Arc::downgrade(&state));
        Ok(FileSnapshotLease { _state: state })
    }
    fn read<'a>(
        self: &'a Arc<Self>,
        lease: &'a Self::SnapshotLease,
        block: BlockId,
    ) -> impl Future<Output = Result<Self::ReadBuf, DeviceError>> + Send + 'a {
        read_impl::read(self, lease, block)
    }
    fn reconcile(
        self: &Arc<Self>,
        expected: Arc<BlockDeviceState>,
    ) -> impl Future<Output = Result<pi_block_device::ReconcileReport, ReconcileError>> + Send + '_
    {
        reconcile_retire::reconcile(self, expected)
    }
    fn enqueue_retirements(
        self: &Arc<Self>,
        objects: Vec<RetiredObject>,
    ) -> Result<(), DeviceError> {
        reclaim_impl::enqueue(self, objects)
    }
    fn reclaim_retirements(
        self: &Arc<Self>,
    ) -> impl Future<Output = Result<RetirementReport, DeviceError>> + Send + '_ {
        reclaim_impl::reclaim(self)
    }
}

/// 公开设备类型入口。
pub mod device {
    pub use crate::{FileBlockDevice, FileBlockDeviceConfig, FileDeviceError};
}
