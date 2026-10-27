# pi_block_device workspace

This repository is a Cargo workspace with three crates:

| Crate | Role |
| --- | --- |
| [pi_block_device](crates/pi_block_device/README.md) | Fixed block-device protocol and value types |
| [pi_file_block_device](crates/pi_file_block_device/README.md) | File-backed device implementation |
| [pi_superblock](crates/pi_superblock/README.md) | Registry, transactions, recovery, and metadata |

The former root package has been removed. The new layout does not migrate persisted data from the 0.1 implementation. Historical design documents remain under `docs/plans/`; historical reports under `docs/reports/` are not current acceptance evidence. Crate-specific release notes are in each crate's `CHANGELOG.md`.

## Current status

Unsafe create/open, immutable segment/large mmap, finite reclaim and strict WriteBatch append are implemented. Complete prepare returns Ok only after completeness checks and actual DataAndMetadata sync on the completed append handles; incomplete/error/cancel paths preserve residue and fence reuse. Recovery validates required files, safely repairs uncommitted Active tails, removes eligible orphans and actually syncs required files and manifest before returning success. See the file crate README for test evidence and limits. Registry pi_async_fs remains =0.1.2; Superblock uses user-authorized local pi_append_log =0.4.0 at D:/dev/pi_append_log. No external source changes or new runtime.

On 2026-10-08 the user explicitly narrowed the original fault model: sudden power loss of new filename/rename/remove directory entries is excluded. Operation errors, atomic no-replace publication, cancellation and process-exit recovery remain in scope. Required file DataAndMetadata sync must really succeed before metadata. media_metadata_synced denotes completed operations/file sync required by this model, not parent-directory fsync or a global pending-write barrier.

The reachable Superblock runtime is implemented (11 remaining TODO sites to zero): real initial checkpoint, recovery/resolver/reconcile, all-device snapshot leases, borrowed transactions, exact writes/streams, consuming prepare, one durable Delta, ArcSwap publication and retirement registration. Transaction<'a,S,C,D,R> still borrows concrete resources without owner/TxnID/baseline leases/phase/closed/WriteGuard. Missing writer_guard denotes closed; async operations locally own batches and the writer guard, and only metadata append uncertainty uses a local cancellation fence. APP reference visitor validation, detailed append/late-failure diagnostic carriers, production maintenance signatures remain unapproved or incomplete. Read-time temporary leases do not provide whole-baseline physical protection.

FileBlockDevice create flushes private staging before atomic no-replace publication; cancellation before publication may leave uninitialized staging that open rejects. File reads use real readonly mmap. Reconcile is an open-recovery operation, not online maintenance; existing leases/mappings prohibit destructive repair. The file recovery slice does not implement Superblock commit/log recovery or establish end-to-end durability ordering. Directory-entry sudden-power-loss guarantees are explicitly excluded by the revised model, not supplied by file flush. Historical Feature comments outside 03/04/16 remain declaration-stage records, not evidence of parent-directory fsync.

## Development

File-only process-kill acceptance covers two actual Windows Child.kill + wait scenarios: a prepared prefix with an awaited but unprepared partial next slot, and fully prepared ordinary/large (>1 MiB) data. Both reopen the same device ID, reconcile a parent-authored external State oracle and assert exact mmap payload/padding. The pinned file suite has 69 passing harness tests with all prior cases retained. Prepare-flush cancellation alone uses a fresh current_exe --exact child with Command.env BLOCKING_MAX_THREADS=1; test-only blocking =1.7.0 controls the worker, not native results. Three synchronous-begin/deferred-gate regressions retain their earlier RED/GREEN evidence; this Superblock skeleton round changes no file tests.

Flush cancellation proves actual completed writes, immediate truncate/overwrite Conflict before and after cancelling the consuming prepare future, and the unchanged instance write fence. A cfg(test)-only Drop handoff retains the original append handle; its byte_len awaits the detached actor's file-slot return, without FIFO assumptions or retry/sleep barriers. Equal-length truncate then actually succeeds without changing the oracle bytes; fresh DataAndMetadata flush and same-ID reopen/reconcile succeed with exact raw and mmap payload/padding. This is actor-leased/native-work-waiting-for-worker cancellation, not proof of cancellation inside a syscall, nor observation of the cancelled actor's success result. Native append/mapping/remove inflight cancellation remains untested; mapping cancellation is not an established leak. On 2026-10-12 the private registry/checkpoint/system-change state closure was implemented and independently tested (26 to 11 remaining TODO sites). Superblock create/open/write/commit have real-file regressions and cancellation fencing; complete end-to-end fault/ordering acceptance is still not claimed.

File retirement enqueue only registers/deduplicates; an external scheduler awaits `reclaim_retirements` for a finite, internally asynchronously serialized round. `RetirementReport` reports successful whole-round object outcomes, not queue balance or physical deletion count; confirmed absence counts as completed. The file backend has no production thread, sleep or timer worker.

Confirmed absence requires NotRemovedByOperation, a typed native NotFound source, and a fresh pi_async_fs try_exists returning false. Guarded/retryable objects retain responsibility without blocking later selected objects in the round. Safety assumes a cooperative single writer, monotonic unreused IDs and no active external path replacement; root exclusion and directory-sync durability are not implemented. Unknown retains responsibility and blocks repeated unlink pending recovery adjudication, which is not implemented. Replaceable paths and remote namespaces are outside this Local-only implementation. The queue is memory-only: Superblock enqueues actual retirements only after successful publication; reopen reconciles real media against durable State without enumerating historical IDs. Mapping-create pending cancellation remains unverified.

```console
cargo +nightly-2026-08-17-i686-pc-windows-gnu fmt --all -- --check
cargo +nightly-2026-08-17-i686-pc-windows-gnu check --workspace --lib
```

pi_block_device and pi_file_block_device integration tests have been migrated to their current interfaces. pi_superblock legacy tests now use current batch/COW interfaces. Full workspace all-targets with test-support passes 226 Superblock + 69 file + 25 protocol harness tests (320 distinct total); workspace all-targets strict Clippy passes. Two new public-commit acceptance tests wrap three real FileBlockDevice owners: channel gates before actual consuming prepare prove key-order waiting, unchanged replay/publication until every prepare succeeds, one durable Delta, exact mmap roots/bindings and retirement observing new publication. Partial prepare failure stops the later device, preserves every file byte, closes/releases the writer and recovers via same-ID device reopen/reconcile. First compiled run passed; no production RED is claimed. Metadata Pending cancellation/native Unknown, APP visitor, append/late-source diagnostics and unapproved production maintenance interfaces remain outside this acceptance. Protocol reclaim mock tests are not production evidence; the file suite separately exercises real mapping and deletion. See each crate's README for its API and current limitations.

The repository is licensed under the [MIT License](LICENSE).
