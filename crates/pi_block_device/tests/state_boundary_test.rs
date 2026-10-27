// @test @api: SegmentData, LevelVersions, BlockDeviceState
// @design-doc: design/features/{01-fixed-protocol,13-wire-format,16-architecture-boundaries}.feature
use pi_block_device::{
    AllocationRun, BlockDeviceState, BlockId, LargeBlockData, LevelVersions, RecordSeq,
    SegmentData, SegmentVersion,
};
use rpds::VectorSync;

fn seq(raw: u32) -> RecordSeq {
    RecordSeq::new(raw).unwrap()
}
fn version(raw: u32) -> SegmentVersion {
    SegmentVersion::new(raw).unwrap()
}
fn segment(runs: &[(u32, u32)], releases: &[u32]) -> SegmentData {
    SegmentData {
        allocation_runs: runs
            .iter()
            .map(|&(at, len)| AllocationRun {
                allocated_at: RecordSeq::from_lifecycle_raw(at).unwrap(),
                len,
            })
            .collect(),
        released_at: releases
            .iter()
            .map(|&at| RecordSeq::from_lifecycle_raw(at).unwrap())
            .collect(),
    }
}

#[test]
fn initial_device_has_fifteen_virtual_active_segments_and_first_large_cursor() {
    let state = BlockDeviceState::initial().unwrap();
    assert_eq!(state.levels.len(), 15);
    for level in &state.levels {
        assert_eq!(level.min_version, version(1));
        assert_eq!(level.versions.len(), 1);
        let active = level.versions.last().unwrap().as_ref().unwrap();
        assert!(active.allocation_runs.is_empty());
        assert!(active.released_at.is_empty());
    }
    assert_eq!(state.large_blocks.next_id.raw(), 1);
    assert_eq!(state.large_blocks.entries.size(), 0);
    assert!(state.validate_at(seq(1)));
}

#[test]
fn initial_level_preserves_requested_maximum_version() {
    let level = LevelVersions::initial(version(0x0fff_ffff));
    assert_eq!(level.min_version, version(0x0fff_ffff));
    assert_eq!(level.versions.len(), 1);
    assert!(level.validate());
}

#[test]
fn window_requires_nonempty_active_tail() {
    for versions in [VectorSync::new_sync(), rpds::vector_sync![None]] {
        assert!(
            !LevelVersions {
                min_version: version(1),
                versions
            }
            .validate()
        );
    }
}

#[test]
fn continuous_window_accepts_one_two_and_more_entries_within_version_range() {
    for count in 1..=3 {
        let mut versions = VectorSync::new_sync();
        for _ in 1..count {
            versions.push_back_mut(None);
        }
        versions.push_back_mut(Some(segment(&[], &[])));
        assert!(
            LevelVersions {
                min_version: version(0x0fff_ffff - count + 1),
                versions
            }
            .validate()
        );
    }
}

#[test]
fn window_rejects_version_overflow() {
    let level = LevelVersions {
        min_version: version(0x0fff_ffff),
        versions: rpds::vector_sync![None, Some(segment(&[], &[]))],
    };
    assert!(!level.validate());
}

#[test]
fn sealed_nonempty_segments_and_active_validate_each_lifecycle() {
    let mut level = LevelVersions {
        min_version: version(1),
        versions: rpds::vector_sync![
            Some(segment(&[(1, 1)], &[2])),
            None,
            Some(segment(&[], &[]))
        ],
    };
    assert!(level.validate_at(seq(2)));
    assert!(!level.validate_at(seq(1)));
    level.versions = rpds::vector_sync![Some(segment(&[(1, 1)], &[1]))];
    assert!(!level.validate());
}

#[test]
fn allocation_runs_require_strict_sequence_and_nonzero_length() {
    for runs in [
        &[(1, 0)][..],
        &[(0, 1)],
        &[(1, 1), (1, 1)],
        &[(2, 1), (1, 1)],
    ] {
        let count: u32 = runs.iter().map(|&(_, len)| len).sum();
        assert!(!segment(runs, &vec![0; count as usize]).validate());
    }
    assert!(segment(&[(1, 1), (2, 1)], &[0, 0]).validate());
}

#[test]
fn release_count_must_equal_allocated_slots() {
    assert!(!segment(&[(1, 2)], &[0]).validate());
    assert!(!segment(&[(1, 1)], &[0, 0]).validate());
    assert!(!segment(&[], &[0]).validate());
    assert!(segment(&[], &[]).validate());
}

#[test]
fn allocation_total_rejects_indices_beyond_u32_max_without_wrapping() {
    assert!(!segment(&[(1, u32::MAX), (2, 2)], &[]).validate());
}

#[test]
fn ordinary_block_accepts_highest_index() {
    let block = BlockId::ordinary(14, version(0x0fff_ffff), u32::MAX).unwrap();
    assert_eq!(
        BlockId::from_raw(block.raw()).unwrap().block_index(),
        Some(u32::MAX)
    );
}

#[test]
fn large_cursor_accepts_exhausted_sentinel_and_requires_entries_below_it() {
    let mut state = BlockDeviceState::initial().unwrap();
    let id = pi_block_device::LargeBlockId::new((1_u64 << 60) - 2).unwrap();
    state.large_blocks.next_id = pi_block_device::LargeBlockCursor::new((1_u64 << 60) - 1).unwrap();
    state.large_blocks.entries.insert_mut(
        id,
        LargeBlockData {
            allocated_at: seq(1),
            released_at: seq(2),
            logical_len: u64::MAX,
        },
    );
    assert!(state.validate_at(seq(2)));
    assert!(!state.validate_at(seq(1)));
    state.large_blocks.next_id = pi_block_device::LargeBlockCursor::new(id.raw()).unwrap();
    assert!(!state.validate());
}

#[test]
fn large_lifecycle_accepts_null_release_and_rejects_earlier_release_or_zero_length() {
    let mut data = LargeBlockData {
        allocated_at: seq(2),
        released_at: RecordSeq::NULL,
        logical_len: 1,
    };
    assert!(data.validate_at(seq(2)));
    data.released_at = seq(1);
    assert!(!data.validate());
    data.released_at = RecordSeq::NULL;
    data.logical_len = 0;
    assert!(!data.validate());
}

#[test]
fn each_release_is_later_than_its_own_allocation_and_bounded_by_current() {
    assert!(segment(&[(1, 2), (3, 2)], &[0, 2, 4, 0]).validate_at(seq(4)));
    for releases in [[0, 1, 4, 0], [0, 2, 3, 0], [0, 2, 2, 0]] {
        assert!(!segment(&[(1, 2), (3, 2)], &releases).validate());
    }
    assert!(!segment(&[(1, 1)], &[3]).validate_at(seq(2)));
}

#[test]
fn validate_at_adds_release_upper_bound_not_allocation_upper_bound() {
    assert!(segment(&[(3, 1)], &[0]).validate_at(seq(1)));
    assert!(
        LargeBlockData {
            allocated_at: seq(3),
            released_at: RecordSeq::NULL,
            logical_len: 1
        }
        .validate_at(seq(1))
    );
}

#[test]
fn state_propagates_invalid_level_and_large_lifecycle() {
    let mut state = BlockDeviceState::initial().unwrap();
    state.levels[14].versions = VectorSync::new_sync();
    assert!(!state.validate());
    assert!(!state.validate_at(seq(1)));
    state.levels[14] = LevelVersions::initial(version(1));
    state.large_blocks.entries.insert_mut(
        pi_block_device::LargeBlockId::new(1).unwrap(),
        LargeBlockData {
            allocated_at: seq(1),
            released_at: RecordSeq::NULL,
            logical_len: 0,
        },
    );
    state.large_blocks.next_id = pi_block_device::LargeBlockCursor::new(2).unwrap();
    assert!(!state.validate());
}

#[test]
fn persistent_clone_does_not_mutate_original_and_remains_send_sync() {
    fn require<T: Send + Sync>() {}
    require::<BlockDeviceState>();
    let original = BlockDeviceState::initial().unwrap();
    let mut candidate = original.clone();
    candidate.levels[0].versions.push_back_mut(None);
    assert_eq!(original.levels[0].versions.len(), 1);
    assert!(original.validate());
    assert!(!candidate.validate());
}
