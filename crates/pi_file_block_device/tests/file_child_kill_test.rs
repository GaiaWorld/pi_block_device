//! @test @api: public write/prepare/open/reconcile/read after actual process kill
//! @impl: src/write_impl.rs, src/reconcile_retire.rs, src/read_impl.rs
//! @design-doc: design/features/03-backend-session.feature, design/features/04-reconcile-retire.feature
use bytes::Bytes;
use futures_lite::future::block_on;
use pi_block_device::{
    AllocationRun, BlockDeviceBackend, BlockDeviceState, BlockDeviceWriteBatch, BlockId,
    BlockWriteChunk, BlockWritePosition, LargeBlockCursor, LargeBlockData, LargeBlockId, RecordSeq,
    SegmentVersion,
};
use pi_file_block_device::{FileBlockDevice, FileBlockDeviceConfig};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, mpsc},
    time::Duration,
};

const CHILD_ROOT: &str = "PI_FILE_KILL_CHILD_ROOT";
const CHILD_STAGE: &str = "PI_FILE_KILL_CHILD_STAGE";
const PARTIAL_ACK: &str = "ACK prefix_prepared_slot1_partial_write_awaited_not_flushed";
const PREPARED_ACK: &str = "ACK ordinary_large_all_writes_awaited_prepare_synced";
const LARGE_LEN: u64 = (1 << 20) + 17;

fn slot(index: u32) -> BlockId {
    BlockId::ordinary(0, SegmentVersion::new(1).unwrap(), index).unwrap()
}

fn chunk(block: BlockId, offset: u64, len: u64, bytes: Bytes) -> BlockWriteChunk {
    BlockWriteChunk::new(block, bytes, BlockWritePosition::new(offset, len).unwrap()).unwrap()
}

// External oracle: fixture intent, never reconstructed from product media/logs.
fn oracle(slots: u32, with_large: bool) -> Arc<BlockDeviceState> {
    let mut state = BlockDeviceState::initial().unwrap();
    let mut data = state.levels[0].versions.get(0).unwrap().clone().unwrap();
    data.allocation_runs.push_back_mut(AllocationRun {
        allocated_at: RecordSeq::new(1).unwrap(),
        len: slots,
    });
    for _ in 0..slots {
        data.released_at.push_back_mut(RecordSeq::NULL);
    }
    state.levels[0].versions.set_mut(0, Some(data));
    if with_large {
        state.large_blocks.next_id = LargeBlockCursor::new(2).unwrap();
        state.large_blocks.entries.insert_mut(
            LargeBlockId::new(1).unwrap(),
            LargeBlockData {
                allocated_at: RecordSeq::new(1).unwrap(),
                released_at: RecordSeq::NULL,
                logical_len: LARGE_LEN,
            },
        );
    }
    assert!(state.validate());
    Arc::new(state)
}

fn ordinary_bytes() -> Vec<u8> {
    let mut bytes = vec![0; 4096];
    bytes[..6].copy_from_slice(&[3, 3, 7, 7, 7, 7]);
    bytes
}

struct DropWitness(PathBuf);
impl Drop for DropWitness {
    fn drop(&mut self) {
        std::fs::write(&self.0, b"child ran Drop").unwrap();
    }
}

fn acknowledge_and_block(ack: &str) {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{ack}").unwrap();
    stdout.flush().unwrap();
    drop(stdout);
    // Parent retains its pipe writer. EOF is a fixture failure, not a stop signal.
    let mut byte = [0];
    std::io::stdin().lock().read_exact(&mut byte).unwrap();
    panic!("child must be killed while blocked, not released through stdin");
}

#[test]
fn file_kill_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT) else {
        return; // Normal harness invocation; only the exact subprocess does I/O.
    };
    let root = PathBuf::from(root);
    let _witness = DropWitness(root.join("child-drop-witness"));
    let stage = std::env::var(CHILD_STAGE).unwrap();
    // SAFETY: parent released its device; this child is the only participant.
    let device =
        block_on(unsafe { FileBlockDevice::open(FileBlockDeviceConfig { root }, None) }).unwrap();
    let prefix = Bytes::from(vec![3; 2]);
    let suffix = Bytes::from(vec![7; 4]);
    let mut batch = device.begin_write_batch().unwrap();
    block_on(batch.write(chunk(slot(0), 0, 6, prefix.clone()))).unwrap();
    block_on(batch.write(chunk(slot(0), 2, 6, suffix.clone()))).unwrap();
    if stage == "partial" {
        block_on(batch.prepare()).expect("slot0 actual prepare sync completed");
        let mut batch = device.begin_write_batch().unwrap();
        block_on(batch.write(chunk(slot(1), 0, 6, Bytes::from(vec![8; 2]))))
            .expect("slot1 partial bytes actually awaited; no prepare/flush");
        acknowledge_and_block(PARTIAL_ACK);
        drop(batch);
    } else {
        assert_eq!(stage, "prepared");
        let large_prefix = Bytes::from(vec![9; 1 << 20]);
        let large_suffix = Bytes::from(vec![8; 17]);
        let large = BlockId::large(LargeBlockId::new(1).unwrap());
        block_on(batch.write(chunk(large, 0, LARGE_LEN, large_prefix.clone()))).unwrap();
        block_on(batch.write(chunk(large, 1 << 20, LARGE_LEN, large_suffix.clone()))).unwrap();
        block_on(batch.prepare()).expect("all actual append handles synced");
        let live_batch = device.begin_write_batch().unwrap();
        acknowledge_and_block(PREPARED_ACK);
        drop((live_batch, large_prefix, large_suffix));
    }
    drop((device, prefix, suffix));
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        // Also handles timeout, assertion and read failures without orphaning a child.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn kill_at_ack(root: &Path, stage: &str, expected_ack: &str) {
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "file_kill_child", "--nocapture"])
        // No process-global set_var, including in concurrent parent tests.
        .env(CHILD_ROOT, root)
        .env(CHILD_STAGE, stage)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut child = ChildGuard(child);
    assert!(child.0.stdin.is_some());
    let stdout = child.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) if line.starts_with("ACK ") => {
                    let _ = sender.send(Ok(line));
                }
                Ok(_) => {} // libtest preamble, not fixture acknowledgement.
                Err(error) => {
                    let _ = sender.send(Err(error));
                    break;
                }
            }
        }
    });
    // Deadlock insurance only: elapsed time never determines an I/O stage.
    let ack = receiver
        .recv_timeout(Duration::from_secs(60))
        .expect("child must acknowledge completed stage")
        .expect("stdout read");
    assert_eq!(ack, expected_ack);
    assert!(child.0.try_wait().unwrap().is_none(), "child still alive");
    child.0.kill().expect("real OS process kill");
    let status = child.0.wait().expect("real child wait");
    assert!(
        !status.success(),
        "kill must not be a normal successful exit"
    );
    reader.join().unwrap();
    assert!(
        !root.join("child-drop-witness").exists(),
        "child did not unwind or execute fixture Drop"
    );
}

#[test]
fn killed_live_partial_batch_repairs_only_tail_and_can_prepare_next_slot() {
    let temp = tempfile::tempdir().unwrap();
    let config = FileBlockDeviceConfig {
        root: temp.path().join("device"),
    };
    // SAFETY: isolated parent-owned fixture with no concurrent participants.
    let device = block_on(unsafe { FileBlockDevice::create(config.clone()) }).unwrap();
    let id = device.device_id();
    drop(device);
    let expected = oracle(1, false);
    kill_at_ack(&config.root, "partial", PARTIAL_ACK);
    assert_eq!(
        std::fs::read(config.root.join("segment-00-00000001.bin")).unwrap(),
        [ordinary_bytes(), vec![8; 2]].concat()
    );
    // SAFETY: child has been killed and reaped; parent is sole participant.
    let device = block_on(unsafe { FileBlockDevice::open(config.clone(), Some(id)) }).unwrap();
    assert_eq!(device.device_id(), id);
    let report = block_on(device.reconcile(Arc::clone(&expected))).unwrap();
    assert_eq!(report.progress.planned_actions, 1);
    assert_eq!(report.progress.completed_actions, 1);
    assert_eq!(report.progress.remaining_actions, 0);
    assert!(!report.progress.partial_progress);
    assert!(report.progress.media_metadata_synced);
    assert_eq!(
        std::fs::read(config.root.join("segment-00-00000001.bin")).unwrap(),
        ordinary_bytes()
    );
    let repeat = block_on(device.reconcile(Arc::clone(&expected))).unwrap();
    assert_eq!(repeat.progress.planned_actions, 0);
    assert!(repeat.progress.media_metadata_synced);
    let lease = device.acquire_snapshot_lease(expected).unwrap();
    let prefix = block_on(device.read(&lease, slot(0))).unwrap();
    assert_eq!(prefix.as_ref(), ordinary_bytes());
    let mut batch = device.begin_write_batch().unwrap();
    block_on(batch.write(chunk(slot(1), 0, 3, Bytes::from(vec![6; 3])))).unwrap();
    block_on(batch.prepare()).unwrap();
    let lease = device.acquire_snapshot_lease(oracle(2, false)).unwrap();
    let next = block_on(device.read(&lease, slot(1))).unwrap();
    let mut bytes = vec![0; 4096];
    bytes[..3].copy_from_slice(&[6; 3]);
    assert_eq!(next.as_ref(), bytes);
    assert_eq!(prefix.as_ref(), ordinary_bytes());
}

#[test]
fn killed_after_full_prepare_reopens_same_id_and_maps_ordinary_and_large() {
    let temp = tempfile::tempdir().unwrap();
    let config = FileBlockDeviceConfig {
        root: temp.path().join("device"),
    };
    // SAFETY: isolated parent-owned fixture with no concurrent participants.
    let device = block_on(unsafe { FileBlockDevice::create(config.clone()) }).unwrap();
    let id = device.device_id();
    drop(device);
    let expected = oracle(1, true);
    kill_at_ack(&config.root, "prepared", PREPARED_ACK);
    // SAFETY: child has been killed and reaped; parent is sole participant.
    let device = block_on(unsafe { FileBlockDevice::open(config, Some(id)) }).unwrap();
    assert_eq!(device.device_id(), id);
    let report = block_on(device.reconcile(Arc::clone(&expected))).unwrap();
    assert_eq!(report.progress.planned_actions, 0);
    assert_eq!(report.progress.completed_actions, 0);
    assert_eq!(report.progress.remaining_actions, 0);
    assert!(!report.progress.partial_progress);
    assert!(report.progress.media_metadata_synced);
    let lease = device.acquire_snapshot_lease(expected).unwrap();
    let ordinary = block_on(device.read(&lease, slot(0))).unwrap();
    let large =
        block_on(device.read(&lease, BlockId::large(LargeBlockId::new(1).unwrap()))).unwrap();
    assert_eq!(ordinary.as_ref(), ordinary_bytes());
    assert_eq!(large.as_ref(), [vec![9; 1 << 20], vec![8; 17]].concat());
}
