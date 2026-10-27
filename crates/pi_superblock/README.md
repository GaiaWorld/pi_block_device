# pi_superblock

pi_superblock 在 pi_block_device backend 协议之上提供静态设备注册、应用状态持久化、事务元数据、恢复、固定快照和日志维护。持久元数据由内部 SuperblockAppendLog 写入用户授权本地源码 D:/dev/pi_append_log，精确 version =0.4.0；不是 registry 发布声明。pi_async_fs 直接精确依赖 =0.1.2，日志的 "0.1" 同样解析到该 registry 版本。现用 append/build/visitor/codec/rotate/archive API 经 workspace --lib 编译兼容，无适配实现改动；crate 不公开 log trait，也不依赖具体 backend 实现。

刷新复审：append_confirmed 强制 durable=true 并 await 日志 append；本地日志先 await 底层 append，再 DataAndMetadata flush，正常完成路径符合 0.1.2 合同。rotate 的 flush_append_barrier 只在同一 owner 的操作串行化范围成立，不是全资源屏障。日志 builder 要求调用方排除同 root 的其它 owner/进程/别名，当前 Superblock 根独占未实现；不能把刷新作为该排他性的替代。durable_tests 的 barrier 名称及注入 future 只证明等待/错误分类，不证明跨资源、目录项或掉电持久性。commit/prepare 已实现，旧测试已迁移；未完成的故障专项见下文。

## 当前能力

2026-10-12 runtime 更新优先于下方历史声明：src 11 个 TODO 已接通并清零，公开 production 方法签名不变。真实 FileBlockDevice/FileAppendLog 测试覆盖初始 CP、同 ID 设备重新 open、roots/bindings 与 payload mmap、精确 stream、单次 Delta、未写拒绝、实际 Pending stream/gate 取消、旧 snapshot/ReadBuf 退休保护以及耗尽 open。write/commit 的 future 局部拥有现有 batches/writer_guard；guard None 表示关闭，只有 metadata append 局部 AppendFence，不恢复事务 phase/WriteGuard。

APP 无批准 visitor，未实现引用遍历验证；无默认空 visitor。CommitFailed/CommitStatusUnknown 与 CommitResult 现有公开形状不能携带 append/late source，本轮不新增字段，诊断通道缺口仍阻塞对应验收。旧support/mock/恢复与维护测试已迁移到同步begin_write_batch、消费式prepare、借用TX和rpds COW；最新完整workspace全目标含test-support实测SB226、file69、block25，合计320项不重复harness测试。新增dynamic_commit_ordering_test两项公开commit验收：三个真实FileBlockDevice适配器，逆序write/sync begin归属，channel gate控制真实prepare调用前的await；逐key阻塞时后续prepare不得触发、实际replay记录与旧publication不变，全部真实prepare成功后仅一条durable Delta，seq/root/binding/真实mmap一致，retirement回调看到新publication。key2受控失败阻止key3、保留全部文件字节、关闭TX/释放writer，全部旧owner释放后同ID unsafe reopen/reconcile并重新write/commit/read成功。首次编译成功即GREEN为验收，不称RED；此前BindingName参数类型编译错误不是生产RED。metadata Pending取消、native Unknown动态注入及未批准production maintenance接口仍未验收。以下说明当前实现及验收边界。

- Superblock::create/open：真实初始 checkpoint、日志恢复、设备解析校验和 reconcile 已实现。
- 借用事务骨架：Transaction<'a,S,C,D,R> 只借具体runtime字段和Option<MutexGuard<'a,()>>，无owner SB；baseline/candidate为Arc。无TransactionId/allocator/baseline_leases/custom gate/permit/phase/closed/WriteGuard。write/stream/commit 已实现；局部资源归属处理关闭与取消，metadata append 使用局部恢复围栏；不提供整个事务基准的物理保护。snapshot leases未删。
- 固定快照与读取合同：snapshot 绑定 Arc<SuperblockState> 和所有设备 lease；device_state 查询已实现，open_snapshot 获取全部设备 lease 已实现并经测试。
- retirement：既有 release 计算与恢复入口保留，commit 在确认发布成功后登记实际退休对象。
  enqueue 仅登记去重，由外部调度 await backend 的有限轮 reclaim_retirements；不改变 durable 事务终态，内存队列不是权威持久状态。FileBlockDevice 已实现真实 mmap 读取与有限异步回收，无生产 thread worker；commit 登记及 open 对真实介质的恢复对账已接入。
- 显式日志维护：底层 rotate/archive 方法保留；checkpoint codec已实现，完整维护链未验收，不宣称维护流程可运行。

## 核心 API

本轮按最新批准落盘借用骨架：公有Transaction新增lifetime，begin_write返回Transaction<'_,S,C,D,R>，TransactionId根导出与hidden injection删除；不声称历史56签名不变。未新增尚未批准的 visitor/getter/查询 API。

| 公开壳 | 私有芯 |
| --- | --- |
| src/superblock.rs | src/superblock/implementation.rs；src/superblock/support.rs 保留维护/恢复支持 |
| src/transaction.rs | src/transaction/implementation.rs；既有 allocation.rs、release.rs、write.rs、stream.rs |
| src/registry.rs | src/registry/implementation.rs |
| src/snapshot.rs | src/snapshot/implementation.rs |
| src/application.rs | src/application/implementation.rs |
| src/format/checkpoint.rs | src/format/checkpoint/implementation.rs |
| src/change.rs | src/change/implementation.rs |

InitialDevice、SuperblockLogConfig、ResolveError、CommitResult、StreamWriteError、RootVersion、DeviceRegistration、DeviceSlot/DeviceSlotRef 等输入/结果/错误/借用值类型不是运行时壳；简单 literal 构造和字段访问保留，不机械跨模块转发。ApplicationState::apply 默认公开约定保留，默认实现转私有 helper，仍返回 not_implemented Error。ApplicationEncoder 的 checked put/freeze、名称验证、roots/bindings 查询、分配/释放及退休 diff、header/Delta/log recovery/rotate 真实算法未清空。

尚未批准的 AllocatedBlock block_ref getter、application visitor、snapshot segment_infos/live_blocks 只列待定，不新增 public API。Transaction::read 私有 helper 只借用所需字段，避免 WriteBatch !Sync 破坏 Send future；基准读取使用临时物理 lease。

- InitialDevice<D>：create 输入的 label 与 backend。
- DeviceResolver<D>：open 时把持久 DeviceRegistration 解析为 backend；create 不调用 resolver。
- SuperblockLogConfig：唯一日志根路径、rotation threshold 和 append options。
- Superblock<S, C, D, R>：create/open、status、begin_write、read 和 open_snapshot。
- Transaction<'a, S, C, D, R>：分配、写入、候选绑定查询、事务基准读取、application/system 变化、release 和 commit。
- SuperblockSnapshot<S, D>：固定 state 查询与 snapshot read。

create 和 open 返回 Send + 'static future。backend、write batch 和 read future 的 Send 边界由 pi_block_device 协议约束。

## 事务读取声明

- get_binding 查询 candidate，保留原签名。
- read 保留借 self 的 Send future；private helper只借baseline/candidate/devices/status，不捕整体Transaction，不要求WriteBatch Sync。
- baseline是固定逻辑Arc，candidate初始同一Arc；begin仅Arc clone、首次 mut 使用 Arc::make_mut，不begin State clone。
- 删除事务整体baseline leases，当前不提供基准物理保护。新块/已 release 块拒绝及恢复围栏检查已实现，不转发全局当前视图。
- snapshot自身leases、ReadBuf独立保护和file/mmap/reclaim安全合同保留。publication_access是SB拥有、TX借用的必要协调锁；当前SB read保留锁内获取临时lease，snapshot/commit 接线已实现。

## 提交合同

已实现以下流程：验证 candidate、按 DeviceKey 顺序通过 Option::take 消费批次并 await prepare、单次 metadata append、Durable 后发布 candidate 和登记 retirement。不提供事务身份分配或跨事务凭证身份校验，metadata durable 后发布候选状态，不调用旧终态 API，也没有 PreparedBatch token。

2026-10-08 用户收窄故障模型：排除新文件名、rename/remove 目录项突然掉电未落盘；操作错误、原子 noreplace、取消和进程退出恢复仍在范围内。prepare(self) 成功意味着数据完整、最终布局可恢复定位，必要文件 DataAndMetadata sync 真实成功先于 metadata durable，不表示父目录 fsync。Drop 仅释放资源，不得删除可能已提交的数据；未提交残留由权威恢复状态驱动 reconcile。Unknown 由 pi_superblock 重新 open 裁决，不能按普通失败回滚重试。Superblock 提交集成已有真实多设备排序验收。

## Wire 与恢复

2026-10-12 checkpoint纯状态codec已实现并经独立literal golden/严格边界目标验证；下列完整运行时合同未全部验收。现有错误carrier按用户确认保留，Feature13 InternalValidationKind分类缺口不宣称满足。

- header 固定 8 bytes：magic、system_version、record_kind、flags。
- append-log block sequence 是唯一 durable RecordSeq 来源。
- checkpoint 和 Delta 使用严格 unsigned little-endian 编码，不使用 serde/bincode。
- 每个 append-log structure 的首条完整记录必须是 checkpoint。
- 解码在分配、复制或调用 application codec 前检查 attacker-controlled count 和 length。
- 每条 Delta 在完整 candidate state 上原子 replay，全部变化成功后才发布。

## 状态与错误

SuperblockStatus 包含：

- Ready：允许 read、snapshot 和 begin_write。
- ReadOnlyExhausted：RecordSeq 已无下一合法值；保留 read/snapshot，拒绝写入和维护。
- RecoveryRequired：durability 或本地责任不可安全判定；拒绝新的 read、snapshot 和 write，必须重新 open。

create/open 使用结构化错误保存 capability field、DeviceKey、恢复阶段和底层 source。运行时错误区分 definite commit failure 与 unknown commit status。

## 当前限制

- maintenance 底层 rotate/archive 实现保留，checkpoint/发布接线实，完整维护链未验收。
- retirement 保留 release 计算和恢复补偿代码；FileBlockDevice 已有内存队列与外部 await 的有限异步回收，无后台删除 worker。commit 发布后登记、open 的全设备解析与 reconcile 已实现；未知删除证据等未验边界仍保留。
- snapshot Arc 类型和只读代码保留，拆分表查询已实现；固定视图和 lease 获取已实现并经测试。
- 部分 append-log/format API 暂以 #[doc(hidden)] 暴露给集成测试；本轮全部保留，是否收窄须另行讨论。
- 尚未完成全部 Feature 的 @impl 回填和最终独立实现验收。

## 持久化状态声明

`BlockDeviceState::initial` 已由协议层实现；本轮不改 file/block 实现或新增集合构造逻辑。

- 精确依赖 rpds 1.1.0：`RedBlackTreeMapSync` / `VectorSync` 使用 `ArcTK`（triomphe::Arc）。有序树为 roots、bindings 和 LargeBlocks.entries；版本、allocation_runs、released_at 为 Vector，十五级数组不变。
- DeviceTable 静态 Arc registrations 与动态 VectorSync states 等长；构造校验非空、1,000,000上限、ID/label唯一和State结构。get/iter返回源表借用，state_mut仅crate持久路径更新，不复制registration、不装箱。
- published直接ArcSwap<State>，无OuterArc；SB devices为SmallVec<[Arc<D>;4]>，私有4仅内联容量、超出spill，不公开新const泛型。TX devices借&[Arc<D>]不复制；codec/log/status/publication/协调及维护字段borrow。SB保留R resolver，TX R仅PhantomData最低改动。status 外层 Arc 保留供独立 snapshot 存活，TX 借用原子状态单元，不拥有 Superblock。
- 保持原有按键升序的确定性编码、DeviceKey 次序及 Vector 序号顺序；system_version 和 wire 字段不升级，不使用 rpds serde。Vector 前部裁剪及 min_version 推进已实现。
- 本切片实现RG4/AP1/SN1/CH1/CP7/active_position1；纯状态system change成功后替换candidate，支持多版本窗口与front trim。create/open/open_snapshot/begin、事务准入/read/write/stream/commit 已实现；visitor/getter 未新增。
- 当前生产源码 todo!/unimplemented! 为零；仍不宣称所有接口和故障专项已完成。

## 验证

固定工具链：nightly-2026-08-17-i686-pc-windows-gnu

    cargo +nightly-2026-08-17-i686-pc-windows-gnu fmt --all -- --check
    cargo +nightly-2026-08-17-i686-pc-windows-gnu check --workspace --lib --locked --offline
    cargo +nightly-2026-08-17-i686-pc-windows-gnu test -p pi_file_block_device --all-targets --locked --offline
    cargo +nightly-2026-08-17-i686-pc-windows-gnu test -p pi_block_device --locked --offline
    cargo +nightly-2026-08-17-i686-pc-windows-gnu test -p pi_superblock --lib --locked --offline

最新完整 workspace/all-targets 含 test-support：Superblock 226、file 69、block 25，共 320 passed，零失败/忽略。严格 Clippy -D warnings 和格式检查通过，详细证据见 WORK。

2026-10-12 全目标迁移验收取代上述声明阶段结果：SB --all-targets --features test-support 为 226 passed，workspace --all-targets check零警告，三crate全目标strict Clippy -D warnings通过。P1小游标与最大合法游标证明open不枚举不存在历史对象；P2真实durable release后publication锁poison仍返回CommitResult+RecoveryRequired，不登记旧发布视图的退休对象，reopen由真实reconcile补偿。APP visitor与append/late source诊断缺口不因此关闭。

## 设计资料

- design/features/05-create-validation.feature
- design/features/06-open-recovery.feature
- design/features/07-snapshot-blockref.feature
- design/features/08-transaction-changes.feature
- design/features/09-bytes-stream.feature
- design/features/11-commit-outcomes.feature
- design/features/12-recordseq-maintenance.feature
- design/features/15-release-metadata.feature
- docs/reports/2026-09-09-pi-superblock-tdd-design-changes.md（历史报告，不作为当前声明阶段验收证据）

当前合同见RULES和最新Feature07/08/09决定。TransactionId/allocator/跨事务归属目标已撤销，不再作为待裁决项。精确arc-swap=1.9.2与smallvec=1.15.2经缓存/原锁核实，正常offline resolver只增加直接依赖边，不升级其它包。
