# BDD 规则追溯目录

## 方案 A 声明阶段覆盖说明

当前写入合同以 03-backend-session、08-transaction-changes、09-bytes-stream、11-commit-outcomes、14-three-device-e2e 的方案 A 修订为准。BlockDeviceWriteBatch 仅有 write(&mut self, BlockWriteChunk) 与消费自身的 prepare(self)，返回 Send future；backend 关联类型为 WriteBatch，begin_write_batch(self: &Arc<Self>) 不接收 TransactionId。2026-10-08 用户收窄故障模型：排除新文件名、rename/remove 目录项突然掉电未落盘，操作错误、原子 noreplace、取消和进程退出恢复仍考虑。prepare 成功要求数据完整、最终布局可恢复定位、必要文件 DataAndMetadata sync 真实成功先于 metadata durable，不表示父目录 fsync。Drop 仅释放资源，不删除可能已提交的数据；残留由权威恢复状态驱动 reconcile，Unknown 由 pi_superblock 恢复裁决。

下表与统计是修订前的历史追溯快照，尚未重新生成，不作为当前写入合同或完成证据。涉及 session 三终态、backend 多段/stream、flush/finish 发布、传递 TransactionId 的行均已失效；其余不受影响规则保留。文件后端 write/prepare/reconcile 已有成功真实 I/O 验证；Superblock src 仍有 28 个私有 todo!，运行时与旧集成测试迁移未完成。Transaction Drop自然RAII，WriteGuard/phase/closed/ID/allocator/baseline_leases已按最新批准删除；当前无终态/取消围栏和事务基准物理保护。snapshot/ReadBuf保护不删；28个私有TODO只是骨架简化，非runtime完成。

## 当前分类（不代表实现验收）

最新结构：SB直接ArcSwap<State>+SmallVec<[Arc<D>;4]>，TX生命周期字段borrow及Option<borrowed MutexGuard>，commit &mut self；无新visitor/getter，snapshot leases保留。历史表不作为已撤销ID/phase/closed/WriteGuard机制的当前要求。

- 当前合同：方案 A 的 write/消费式 prepare、prepare 持久化先于 metadata durable、Drop 仅释放资源、Unknown 由恢复裁决；04 的成功 media_metadata_synced=true、保护与重启补偿；16 的依赖边界、多段/流写及 Sealed/large mmap 目标均保留。无 backend stream 签名不取消事务/metadata 层目标，也不恢复旧 session API。
- 当前命令机制：2026-10-11撤销事务phase/closed/WriteGuard及终态矩阵，私有准入保留明确TODO、不返回假Ok；恢复/耗尽真实检查保留，取消围栏未提供，不恢复重复阶段名。
- 已有实现：文件 unsafe create/open、普通段 slot offset 与 Active/Sealed/large 完成范围 mmap、弱映射缓存、有限异步 reclaim 和严格 WriteBatch 追加。prepare 检查数据/补零完整并在实际完成 append 的句柄上 await DataAndMetadata sync，全部成功才返回 Ok。reconcile 验证 required 文件、保护性修复 Active 权威边界外尾部、清理可删除孤儿，并真实同步 required 文件及 manifest 后返回 synced=true；错误/取消保持围栏。旧线程移除，ReadBuf 保留 lease。文件切片成功不等于 Superblock 端到端验收。
- 待实现差距：目录 fsync/root 独占未实现；目录项突然掉电未落盘已被用户明确排除，不以文件 sync 冒充目录同步。registry 精确 pi_async_fs =0.1.2 已实测映射存活时两种强 flush、await append 和继续追加/新范围读取成功；flush 不覆盖其它资源未完成写入，不是全局屏障。mapping 创建 pending/取消仍未验证。Superblock 保留 28 个私有 todo!，Drop自然RAII但取消/终态协议未提供，运行时/旧集成测试仍待迁移；metadata append 仍使用单一 encoded buffer。日志来源为用户授权 D:/dev/pi_append_log =0.4.0 本地源码，以下 0.3 历史索引不是当前版本证据。
- 待裁决：事务身份/allocator/跨事务归属目标已由用户撤销，不再待裁决；16 的 futures-core/futures-lite 依赖归属与旧测试迁移路径。futures-core 三 crate 均声明，底层 src 未直接引用但旧 tests 引用，superblock Stream 签名仍引用；文件后端生产代码实际使用 futures-lite。保留冻结依赖规则，不因 Cargo 现状降低要求。

历史报告（包括 docs/reports）和以下索引/统计均不作为当前验收依据；本轮未重建统计、未回填或声称任何 @impl 通过。待裁决项不授权改实现或冻结目标。

## 历史追溯快照

以下历史目录由修订前的 16 个 `.feature` 文件解析，非当前重新生成结果：Scenario/Scenario Outline 按定义计数，Outline 实例按所有 Examples 数据行展开，`critical` 按场景前实际 `@critical` 标签计数。
批准设计：`docs/plans/2026-09-02-block-device-superblock-split-design.md`

统计：feature 16，规则 ID 356，Scenario/Outline 定义 349（其中 Outline 79），Examples 展开后实例 653，critical 定义 81，critical 实例 198，non-critical 定义 268。
规则与场景不是 1:1：10-application.feature: 25 rules / 19 scenarios; unmapped rules AP-20, AP-21, AP-22, AP-23, AP-24, AP-25；13-wire-format.feature: 42 rules / 41 scenarios; unmapped rules WF-42。除这些 7 个未映射规则外，其余规则按各 feature 顶部 `# @rules` 顺序对应场景定义顺序；不得据此声称全量精确对应。

| ID | critical | 来源 | 一句话可验证规则 | 对应 Feature + Scenario/Scenario Outline | Examples rows |
| --- | --- | --- | --- | --- | ---: |
| FP-01 | yes | `design/features/01-fixed-protocol.feature` | level、segment_version 和 block_index 恢复为原值 | Feature: 固定块协议 + Scenario: 普通 BlockId 保持既有位布局 | 1 |
| FP-02 | no | `design/features/01-fixed-protocol.feature` | 校验返回 InvalidBlockId | Feature: 固定块协议 + Scenario: 普通 BlockId 拒绝零 SegmentVersion | 1 |
| FP-03 | no | `design/features/01-fixed-protocol.feature` | 三个字段均恢复为输入的最大合法值 | Feature: 固定块协议 + Scenario: 普通 BlockId 接受最大合法字段 | 1 |
| FP-04 | no | `design/features/01-fixed-protocol.feature` | 校验返回 InvalidBlockId | Feature: 固定块协议 + Scenario: 全一位 BlockId 保留值被拒绝 | 1 |
| FP-05 | yes | `design/features/01-fixed-protocol.feature` | base size 为 4096 字节且表为 [4096, 6144, 8192, 12288, 16384, 24576, 32768, 49152, 65536, 98304, 131072, 196608, 262144, 393216, 524288] 并且 independent large block 为 true | Feature: 固定块协议 + Scenario: 固定几何完整匹配十五项 KiB 表 | 1 |
| FP-06 | no | `design/features/01-fixed-protocol.feature` | level 为 15 且 high 60 bits 恢复为 42 | Feature: 固定块协议 + Scenario: level 15 使用独立大块 ID | 1 |
| FP-07 | no | `design/features/01-fixed-protocol.feature` | 校验返回 InvalidBlockId | Feature: 固定块协议 + Scenario: large block 拒绝零 ID | 1 |
| FP-08 | no | `design/features/01-fixed-protocol.feature` | large_block_id 恢复为 2^60-2 | Feature: 固定块协议 + Scenario: large block 接受最大有效 ID | 1 |
| FP-09 | no | `design/features/01-fixed-protocol.feature` | 分配被拒绝且 next_id 不回绕 | Feature: 固定块协议 + Scenario: large block 耗尽哨兵不能分配 | 1 |
| FP-10 | yes | `design/features/01-fixed-protocol.feature` | 输出依次为四字节 DeviceKey、八字节 BlockId 的 little-endian 字节且长度为 12 | Feature: 固定块协议 + Scenario: BlockRef 使用十二字节小端编码 | 1 |
| FP-11 | no | `design/features/01-fixed-protocol.feature` | 0 是 Null、1..=u32::MAX-1 是有效记录且 u32::MAX 是耗尽哨兵 | Feature: 固定块协议 + Scenario: RecordSeq 有效范围排除 Null 和耗尽哨兵 | 1 |
| FP-12 | no | `design/features/01-fixed-protocol.feature` | 返回 RecordSeqExhausted 且不生成 0 | Feature: 固定块协议 + Scenario: 生命周期记录不允许回绕 | 1 |
| RG-01 | yes | `design/features/02-registry-capabilities.feature` | 返回 InvalidConfiguration | Feature: 静态注册表与能力校验 + Scenario: 注册表拒绝零设备 | 1 |
| RG-02 | no | `design/features/02-registry-capabilities.feature` | registry 包含 DeviceKey 1 且 create 继续初始化 | Feature: 静态注册表与能力校验 + Scenario: 注册表接受一个设备 | 1 |
| RG-03 | no | `design/features/02-registry-capabilities.feature` | 数量校验成功且最大 DeviceKey 为 1000000 | Feature: 静态注册表与能力校验 + Scenario: 注册表接受一百万个设备 | 1 |
| RG-04 | yes | `design/features/02-registry-capabilities.feature` | 返回 InvalidConfiguration | Feature: 静态注册表与能力校验 + Scenario: 注册表拒绝超过一百万个设备 | 1 |
| RG-05 | no | `design/features/02-registry-capabilities.feature` | 返回 InvalidConfiguration | Feature: 静态注册表与能力校验 + Scenario: DeviceRegistration 拒绝零 BlockDeviceId | 1 |
| RG-06 | no | `design/features/02-registry-capabilities.feature` | label 按原始 UTF-8 字节恢复且不执行规范化 | Feature: 静态注册表与能力校验 + Scenario: DeviceLabel 使用原始 UTF-8 字节 | 1 |
| RG-07 | no | `design/features/02-registry-capabilities.feature` | 校验结果为 <result> | Feature: 静态注册表与能力校验 + Scenario Outline: DeviceLabel 长度边界精确校验 | 4 |
| RG-08 | yes | `design/features/02-registry-capabilities.feature` | 返回 InvalidConfiguration | Feature: 静态注册表与能力校验 + Scenario: DeviceLabel 重复时拒绝注册表 | 1 |
| RG-09 | no | `design/features/02-registry-capabilities.feature` | 返回 InvalidConfiguration | Feature: 静态注册表与能力校验 + Scenario: BlockDeviceId 重复时拒绝注册表 | 1 |
| RG-10 | no | `design/features/02-registry-capabilities.feature` | registry 校验成功 | Feature: 静态注册表与能力校验 + Scenario: hot 和 Hot label 可以共存 | 1 |
| RG-11 | no | `design/features/02-registry-capabilities.feature` | 三个设备依次获得 DeviceKey 1、2、3 | Feature: 静态注册表与能力校验 + Scenario: DeviceKey 按输入顺序稳定分配 | 1 |
| RG-12 | no | `design/features/02-registry-capabilities.feature` | 操作不存在或被拒绝，Vec 长度和 DeviceKey 到 slot 的 checked index 映射保持不变，且原有 Arc backend 不被替换 | Feature: 静态注册表与能力校验 + Scenario: 静态设备表禁止动态变更 | 1 |
| RG-13 | yes | `design/features/02-registry-capabilities.feature` | 返回 CreateError::NotCreated、kind=IncompatibleDeviceCapabilities、保留 DeviceKey <key> 和 source | Feature: 静态注册表与能力校验 + Scenario Outline: create 能力字段不匹配返回 NotCreated | 4 |
| RG-14 | no | `design/features/02-registry-capabilities.feature` | 返回 OpenError::RecoveryFailed、kind=IncompatibleDeviceCapabilities、保留 DeviceKey <key> 和 source | Feature: 静态注册表与能力校验 + Scenario Outline: open 能力字段不匹配返回 RecoveryFailed | 4 |
| RG-15 | no | `design/features/02-registry-capabilities.feature` | capability 校验继续且不返回 IncompatibleDeviceCapabilities | Feature: 静态注册表与能力校验 + Scenario Outline: optional capability flags 被 S3 判定兼容时继续 | 2 |
| RG-16 | no | `design/features/02-registry-capabilities.feature` | 当 operation=create 时返回 CreateError::NotCreated、kind=IncompatibleDeviceCapabilities、保留 DeviceKey <key> 和 source；当 operation=open 时返回 OpenError::RecoveryFailed、kind=IncompatibleDeviceCapabilities、保留 DeviceKey <key> 和 source | Feature: 静态注册表与能力校验 + Scenario Outline: optional capability flags 被 S3 判定不兼容时保持外层错误 | 2 |
| BS-01 | yes | `design/features/03-backend-session.feature` | 返回 future 满足 Send 且 backend 满足 Send、Sync 和 'static | Feature: Backend 与 session 终态 + Scenario: backend 异步 future 满足 Send | 1 |
| BS-02 | yes | `design/features/03-backend-session.feature` | 不存在持久对象、marker、queue、reservation 且 RAII 资源全部释放 | Feature: Backend 与 session 终态 + Scenario Outline: begin_write 各阶段取消不产生持久副作用 | 4 |
| BS-03 | no | `design/features/03-backend-session.feature` | writer gate 被释放且不存在 backend session 或持久副作用 | Feature: Backend 与 session 终态 + Scenario: Superblock begin_write 取消只释放 gate | 1 |
| BS-04 | no | `design/features/03-backend-session.feature` | session 被消费且全部临时资源释放 | Feature: Backend 与 session 终态 + Scenario: WriteSession abandon 消费自身 | 1 |
| BS-05 | no | `design/features/03-backend-session.feature` | session 被消费并报告 finish 成功 | Feature: Backend 与 session 终态 + Scenario: WriteSession finish 消费自身 | 1 |
| BS-06 | yes | `design/features/03-backend-session.feature` | session 被消费、资源进入恢复责任且 preserve 返回成功 | Feature: Backend 与 session 终态 + Scenario: WriteSession preserve 成功保持可恢复资源 | 1 |
| BS-07 | no | `design/features/03-backend-session.feature` | 后续 abandon、finish 和 preserve 均不可再次消费同一 session 且重复终结调用编译失败 | Feature: Backend 与 session 终态 + Scenario Outline: session 终态互斥且类型禁止重复终结 | 3 |
| BS-08 | yes | `design/features/03-backend-session.feature` | 物理临时工作被 abandon 且不进入 preserve 状态 | Feature: Backend 与 session 终态 + Scenario: 非 append 异常 Drop 执行 abandon | 1 |
| BS-09 | no | `design/features/03-backend-session.feature` | 不执行 abandon 且保留恢复责任 | Feature: Backend 与 session 终态 + Scenario: append 未知后的异常 Drop 保留 session | 1 |
| BS-10 | no | `design/features/03-backend-session.feature` | InspectReport.empty=false 且目录对象、内容和介质元数据逐项不变 | Feature: Backend 与 session 终态 + Scenario: inspect 对非空介质零修改并报告 empty false | 1 |
| BS-11 | no | `design/features/03-backend-session.feature` | 该调用路径不存在 | Feature: Backend 与 session 终态 + Scenario: inspect 仅由 create 调用 | 1 |
| BS-12 | no | `design/features/03-backend-session.feature` | InspectReport.empty 为 false 且 create 返回外层 CreateError::NotCreated、kind 为 DeviceNotEmpty、source=None | Feature: Backend 与 session 终态 + Scenario Outline: inspect 对象类别统一报告非空 | 6 |
| BS-13 | no | `design/features/03-backend-session.feature` | InspectReport.empty 为 true 且 create 不返回 DeviceNotEmpty | Feature: Backend 与 session 终态 + Scenario: inspect 空后允许 create 继续 | 1 |
| BS-14 | no | `design/features/03-backend-session.feature` | <assertion> | Feature: Backend 与 session 终态 + Scenario Outline: BlockWritePosition 和 BlockWriteChunk checked 边界及 roundtrip | 8 |
| BS-29 | no | `design/features/03-backend-session.feature` | backend 使用 pi-async-fs 按顺序完成多段写、每段只写自身 range、整体 logical_len 精确匹配且可由 snapshot lease 读取 | Feature: Backend 与 session 终态 + Scenario: session 多段写按连续 checked request 一次提交 | 1 |
| BS-30 | no | `design/features/03-backend-session.feature` | <result> | Feature: Backend 与 session 终态 + Scenario Outline: session 异步流写精确消费并拒绝长度错误 | 3 |
| BS-15 | yes | `design/features/03-backend-session.feature` | 创建非零长度 segment、数据位于目标 slot、介质元数据 durable 且该 Active 可由 reconcile 识别为已物化 | Feature: Backend 与 session 终态 + Scenario: 虚拟空 Active 首次普通块 write_at 成功物化 segment | 1 |
| BS-16 | yes | `design/features/03-backend-session.feature` | 临时物化工作被清理或隔离、没有可被视为成功 segment 的零长对象、transaction closed 且 gate released | Feature: Backend 与 session 终态 + Scenario Outline: 虚拟空 Active 首次物化失败或取消不留零长成功对象 | 6 |
| BS-17 | no | `design/features/03-backend-session.feature` | 对外结果保持 <public_result>、记录 DeviceKey 和 DeviceError、实例进入 RecoveryRequired 且该 session 不调用其他终态 | Feature: Backend 与 session 终态 + Scenario Outline: 三终态返回 Err 保持阶段对应公开结果 | 3 |
| BS-18 | no | `design/features/03-backend-session.feature` | panic 向调用方传播、记录 DeviceKey 和 panic 诊断、实例进入 RecoveryRequired 且该 session 不调用其他终态 | Feature: Backend 与 session 终态 + Scenario Outline: 三终态 panic 后不尝试其他终态 | 3 |
| BS-19 | yes | `design/features/03-backend-session.feature` | 返回可用 FileBlockDevice、manifest 持久化非零 BlockDeviceId 且再次 create 拒绝并不覆盖现有介质 | Feature: Backend 与 session 终态 + Scenario: 文件后端 create 生成并持久化稳定身份 | 1 |
| BS-20 | yes | `design/features/03-backend-session.feature` | 成功时 device_id 等于 manifest 身份且所有路径均不创建、截断、重写或删除已有介质 | Feature: Backend 与 session 终态 + Scenario: 文件后端 open 恢复身份且保持非破坏 | 1 |
| BS-21 | yes | `design/features/03-backend-session.feature` | 返回 FileDeviceError::<error> 且路径集合、文件内容、长度和介质元数据逐项不变 | Feature: Backend 与 session 终态 + Scenario Outline: 文件后端 create 和 open 拒绝非法配置或路径且零修改 | 6 |
| BS-22 | no | `design/features/03-backend-session.feature` | manifest 原子持久化 format_version、固定 protocol_version、非零 BlockDeviceId、base_block_size、十五项 level_sizes 和 independent_large_blocks=true，且返回 capabilities 等于 BackendCapabilities::fixed() | Feature: Backend 与 session 终态 + Scenario: 文件后端 create 成功写入固定格式 manifest | 1 |
| BS-23 | yes | `design/features/03-backend-session.feature` | 返回 FileDeviceError::InvalidManifest 且 manifest、数据对象和目录项逐字节不变 | Feature: Backend 与 session 终态 + Scenario Outline: 文件后端 open 拒绝非法 manifest 且不修复 | 7 |
| BS-24 | yes | `design/features/03-backend-session.feature` | 返回 FileDeviceError::IdentityMismatch 且不创建、截断、重写、删除或修复任何介质对象 | Feature: Backend 与 session 终态 + Scenario: 文件后端 open 拒绝 caller expected identity mismatch | 1 |
| BS-25 | no | `design/features/03-backend-session.feature` | 两次均返回 device_id=A 且 capabilities 等于 BackendCapabilities::fixed() | Feature: Backend 与 session 终态 + Scenario: 文件后端 open 接受匹配或未指定 expected identity | 1 |
| BS-26 | yes | `design/features/03-backend-session.feature` | 既有路径、manifest、数据对象、内容和长度逐项不变，且未发布的 create 临时文件被清理或保持不可被 open 识别 | Feature: Backend 与 session 终态 + Scenario Outline: 文件后端 create 和 open 失败或取消保持调用前介质 | 6 |
| BS-28 | no | `design/features/03-backend-session.feature` | 精确映射依次为 InvalidConfiguration、AlreadyExists 或 NotFound、InvalidManifest、IdentityMismatch、Io source 且不新增隐式能力不匹配分支 | Feature: Backend 与 session 终态 + Scenario: 文件后端错误映射保持冻结 | 1 |
| BS-27 | yes | `design/features/03-backend-session.feature` | capabilities 恒等于 BackendCapabilities::fixed() 且 config 只有 root、manifest 不持久化调用方可选 capabilities 或 optional flags | Feature: Backend 与 session 终态 + Scenario: 文件后端能力固定且调用方不能配置 | 1 |
| RC-01 | no | `design/features/04-reconcile-retire.feature` | reconcile 成功且不物化该空 Active | Feature: 文件 reconcile 与 retire + Scenario: 虚拟空 Active 缺少文件时成功 reconcile | 1 |
| RC-02 | no | `design/features/04-reconcile-retire.feature` | 该孤儿被删除且 remaining_actions 为零 | Feature: 文件 reconcile 与 retire + Scenario: 零长空段孤儿被删除 | 1 |
| RC-03 | yes | `design/features/04-reconcile-retire.feature` | ReconcileError.kind 为 RequiredObjectMissing 且保留 source | Feature: 文件 reconcile 与 retire + Scenario: 非空 Active 缺失返回 RequiredObjectMissing | 1 |
| RC-04 | no | `design/features/04-reconcile-retire.feature` | ReconcileError.kind 为 RequiredObjectCorrupt 且保留 source | Feature: 文件 reconcile 与 retire + Scenario: 非空 Active 损坏返回 RequiredObjectCorrupt | 1 |
| RC-05 | no | `design/features/04-reconcile-retire.feature` | ReconcileError.kind 为 RequiredObjectMissing 且保留 source | Feature: 文件 reconcile 与 retire + Scenario: Sealed 缺失返回 RequiredObjectMissing | 1 |
| RC-06 | yes | `design/features/04-reconcile-retire.feature` | ReconcileError.kind 为 RequiredObjectCorrupt 且保留 source | Feature: 文件 reconcile 与 retire + Scenario: Sealed 损坏返回 RequiredObjectCorrupt | 1 |
| RC-07 | no | `design/features/04-reconcile-retire.feature` | completed_actions=P、remaining_actions=0、partial_progress=false 且 media_metadata_synced=true | Feature: 文件 reconcile 与 retire + Scenario: reconcile 成功进度满足完整不变量 | 1 |
| RC-08 | no | `design/features/04-reconcile-retire.feature` | kind=RepairIo、completed_actions=C、remaining_actions=P-C、partial_progress=(C>0)、media_metadata_synced=false 且保留 source | Feature: 文件 reconcile 与 retire + Scenario: RepairIo 失败报告完整进度和 source | 1 |
| RC-09 | no | `design/features/04-reconcile-retire.feature` | kind=MediaMetadataSync、completed_actions=C、remaining_actions=P-C、partial_progress=(C>0)、media_metadata_synced=false 且保留 source | Feature: 文件 reconcile 与 retire + Scenario: MediaMetadataSync 失败不计入 completed | 1 |
| RC-10 | no | `design/features/04-reconcile-retire.feature` | completed_actions=C、remaining_actions=P-C、partial_progress=true 且保留 source | Feature: 文件 reconcile 与 retire + Scenario: 部分完成失败满足 progress 数值不变量 | 1 |
| RC-11 | no | `design/features/04-reconcile-retire.feature` | ReconcileError.kind 为 ConcurrentMutation 且保留 source | Feature: 文件 reconcile 与 retire + Scenario: 并发介质变更返回 ConcurrentMutation | 1 |
| RC-12 | yes | `design/features/04-reconcile-retire.feature` | commit 返回 Ok(CommitResult) 且 status=RecoveryRequired 并保留 DeviceError source | Feature: 文件 reconcile 与 retire + Scenario: enqueue 失败使 durable commit 进入恢复围栏 | 1 |
| RC-13 | no | `design/features/04-reconcile-retire.feature` | 调用同步返回且 retire worker 可在之后异步删除 | Feature: 文件 reconcile 与 retire + Scenario: enqueue 成功不等待 worker 删除 | 1 |
| RC-14 | yes | `design/features/04-reconcile-retire.feature` | 文件继续存在直到保护资源全部释放 | Feature: 文件 reconcile 与 retire + Scenario: retire worker 被旧 snapshot 阻塞 | 1 |
| RC-15 | no | `design/features/04-reconcile-retire.feature` | open 重新登记未完成的 retirement 且返回 Ready | Feature: 文件 reconcile 与 retire + Scenario: enqueue 失败由重启补偿 | 1 |
| RC-16 | no | `design/features/04-reconcile-retire.feature` | 从 E 和当前介质重新生成完整计划且重复动作幂等 | Feature: 文件 reconcile 与 retire + Scenario: reconcile 失败后从同一 expected 重新规划 | 1 |
| RC-17 | no | `design/features/04-reconcile-retire.feature` | open 返回完整实例、reconcile 总调用次数恰为 <success_attempt>、成功后不执行额外尝试、每次均从 E 和当次实际介质从头生成完整计划、不把 progress 作为续跑游标，且最多三次是首版内部默认而不是公共 API 承诺 | Feature: 文件 reconcile 与 retire + Scenario Outline: open 可重试 reconcile 在第 1、2 或 3 次成功后立即停止 | 7 |
| RC-18 | no | `design/features/04-reconcile-retire.feature` | 返回 OpenError::RecoveryFailed、kind=Reconcile、source 来自最后一次尝试、不执行第四次 reconcile，且每次均从 E 和当次实际介质从头生成完整计划 | Feature: 文件 reconcile 与 retire + Scenario: open 首版默认三次可重试 reconcile 均失败后停止 | 1 |
| RC-19 | no | `design/features/04-reconcile-retire.feature` | inspected_objects 已记录、planned_actions=0、completed_actions=0、remaining_actions=0、partial_progress=false 且 media_metadata_synced=true | Feature: 文件 reconcile 与 retire + Scenario: 虚拟空 Active 成功报告完整 progress | 1 |
| RC-20 | no | `design/features/04-reconcile-retire.feature` | 缺少 segment 合法、临时或零长孤儿被删除、不得把残留提升为已物化 segment 且 reconcile 可幂等重试 | Feature: 文件 reconcile 与 retire + Scenario Outline: 首次物化失败残留由权威 expected State 裁决 | 6 |
| RC-21 | no | `design/features/04-reconcile-retire.feature` | 立即返回 OpenError::RecoveryFailed、kind=Reconcile、保留首次 source、不执行第二次 reconcile 且不得重建、替换或提升 required object | Feature: 文件 reconcile 与 retire + Scenario Outline: required object reconcile 错误立即失败且不重建 | 2 |
| RC-22 | no | `design/features/04-reconcile-retire.feature` | 按 S3裁决 RECONCILE-RETRY=仅三类瞬态最多三次且取消不计次不续跑 的取消裁决停止、不把 progress 作为续跑游标、不自动开始下一次尝试，且后续显式 open 若获准重试仍从 E 和届时实际介质从头生成完整计划 | Feature: 文件 reconcile 与 retire + Scenario Outline: reconcile 取消按最终取消裁决停止且不自动续跑 | 3 |
| CR-01 | no | `design/features/05-create-validation.feature` | 返回 CreateError::NotCreated、kind=<kind> 且 source=Some(error) | Feature: create 校验与取消 + Scenario Outline: create 失败 source Some 矩阵 | 5 |
| CR-02 | no | `design/features/05-create-validation.feature` | 返回 CreateError::NotCreated、kind=<kind> 且 source=None | Feature: create 校验与取消 + Scenario Outline: create 失败 source None 矩阵 | 3 |
| CR-03 | yes | `design/features/05-create-validation.feature` | create 不调用任何 backend begin_write、write_at 或 flush | Feature: create 校验与取消 + Scenario: create 不调用 backend begin_write | 1 |
| CR-04 | no | `design/features/05-create-validation.feature` | append-log envelope block_seq 为 1，superblock 从其注入 RecordSeq=1，8 字节 header 不保存 sequence、application metadata 或 body length，body 依次包含静态 slot registry、每设备 BlockDeviceState、空 roots、空 bindings 和 application payload | Feature: create 校验与取消 + Scenario: create 首 checkpoint 编码完整初始 State | 1 |
| CR-05 | no | `design/features/05-create-validation.feature` | 每设备恰有 15 个 min_version=1、唯一 Some 尾项且 allocation_runs 和 released_at 为空的虚拟 Active，large entries 为空且 next_id 为首个合法 ID，并且不创建或 flush 物理 segment | Feature: create 校验与取消 + Scenario: create 为每个设备建立十五个虚拟 Active | 1 |
| CR-06 | yes | `design/features/05-create-validation.feature` | 调用方必须先调用 open 再决定是否重新 create | Feature: create 校验与取消 + Scenario Outline: create future 各阶段取消要求先 open | 6 |
| CR-07 | no | `design/features/05-create-validation.feature` | create 不返回 CreateError 且调用方必须先 open | Feature: create 校验与取消 + Scenario: append 开始后的 create 取消要求 open 裁决 | 1 |
| CR-08 | no | `design/features/05-create-validation.feature` | create 不返回 CreateError 且调用方必须先 open | Feature: create 校验与取消 + Scenario: append 未开始的 create 取消要求 open 裁决 | 1 |
| CR-09 | no | `design/features/05-create-validation.feature` | 返回 CreateError::StatusUnknown 且保留 source | Feature: create 校验与取消 + Scenario: create append Unknown 同步失败 | 1 |
| CR-10 | yes | `design/features/05-create-validation.feature` | 所有设备操作只使用 InitialDevice.device 的 Arc<D>、resolver 调用次数为零且 resolver 的缺失映射或失败不影响 create 结果 | Feature: create 校验与取消 + Scenario: create 只使用 InitialDevice 中的 Arc backend | 1 |
| CR-11 | yes | `design/features/05-create-validation.feature` | create 进入正常校验并可成功写入唯一初始 checkpoint、sequence 从 1 开始、不继承已取消 create 的 reservation、临时对象、participant 或 writer gate，且不产生重复 registry、重复 checkpoint 或重复物理初始化 | Feature: create 校验与取消 + Scenario: create 取消后 open 返回 NotInitialized 允许重新 create | 1 |
| CR-12 | yes | `design/features/05-create-validation.feature` | 外层返回 CreateError，返回 CreateError::NotCreated { kind: LogPreparation, source: Some(original recovery evidence/error) }，且不得把介质视为空、不得执行日志初始化、truncate、replace、append 或新 checkpoint 写入、不得删除 preserved object、未知 append 结果或可恢复日志、不得调用会覆盖或清理已有介质事实的 backend 操作且原 RecoveryFailed 恢复证据保持不变 | Feature: create 校验与取消 + Scenario: create 取消后 open 返回 RecoveryFailed 禁止重新 create 覆盖介质 | 1 |
| OP-01 | yes | `design/features/06-open-recovery.feature` | 外层返回 RecoveryFailed 且 kind 为 ResolveDevice 并保留 source | Feature: open、resolver 与恢复裁决 + Scenario: resolver 失败只映射 ResolveDevice | 1 |
| OP-02 | no | `design/features/06-open-recovery.feature` | 外层返回 RecoveryFailed 且 kind 为 DeviceIdentityMismatch 并保留 source | Feature: open、resolver 与恢复裁决 + Scenario: backend 身份不匹配映射 DeviceIdentityMismatch | 1 |
| OP-03 | no | `design/features/06-open-recovery.feature` | 外层返回 RecoveryFailed 且 kind 为 IncompatibleDeviceCapabilities 并保留 source | Feature: open、resolver 与恢复裁决 + Scenario: backend 能力不匹配映射 IncompatibleDeviceCapabilities | 1 |
| OP-04 | no | `design/features/06-open-recovery.feature` | 外层返回 RecoveryFailed 且 kind 为 Reconcile 并保留 source | Feature: open、resolver 与恢复裁决 + Scenario: reconcile 错误映射 Reconcile | 1 |
| OP-05 | yes | `design/features/06-open-recovery.feature` | open 返回包含三个设备的 Ready Superblock | Feature: open、resolver 与恢复裁决 + Scenario: 全部设备在线后 open 才成功 | 1 |
| OP-06 | yes | `design/features/06-open-recovery.feature` | 每个 backend 仅由 resolver 返回的 Arc<D> 提供且 open 不从 registry wire 读取 Path、URI 或 locator | Feature: open、resolver 与恢复裁决 + Scenario: open 仅通过外部 resolver 映射 registry 设备位置 | 1 |
| OP-07 | no | `design/features/06-open-recovery.feature` | open 返回 Ready Superblock | Feature: open、resolver 与恢复裁决 + Scenario: 有效日志恢复返回 Ready | 1 |
| OP-08 | no | `design/features/06-open-recovery.feature` | 外层返回 NotInitialized 且不返回 Superblock | Feature: open、resolver 与恢复裁决 + Scenario: 空容器返回 NotInitialized | 1 |
| OP-09 | yes | `design/features/06-open-recovery.feature` | 外层返回 RecoveryFailed 且 kind 为 Decode 并保留 source | Feature: open、resolver 与恢复裁决 + Scenario: 损坏帧返回 Decode | 1 |
| OP-10 | no | `design/features/06-open-recovery.feature` | 外层返回 RecoveryFailed 且 kind 为 UnsupportedFormat 并保留 source | Feature: open、resolver 与恢复裁决 + Scenario: 不支持格式返回 UnsupportedFormat | 1 |
| OP-11 | no | `design/features/06-open-recovery.feature` | 外层返回 RecoveryFailed 且 kind 为 ApplicationFormatMismatch 并保留 source | Feature: open、resolver 与恢复裁决 + Scenario: application 格式不匹配返回 ApplicationFormatMismatch | 1 |
| OP-12 | no | `design/features/06-open-recovery.feature` | 外层返回 RecoveryFailed 且 kind 为 ApplicationReplay 并保留 source | Feature: open、resolver 与恢复裁决 + Scenario: application replay 失败返回 ApplicationReplay | 1 |
| OP-13 | no | `design/features/06-open-recovery.feature` | 外层返回 RecoveryFailed 且 kind 为 Reconcile 并保留 source | Feature: open、resolver 与恢复裁决 + Scenario: reconcile 失败返回 ReconcileFailure source | 1 |
| OP-14 | no | `design/features/06-open-recovery.feature` | 外层 kind 为 DeviceIdentityMismatch 而不是 ResolveDevice | Feature: open、resolver 与恢复裁决 + Scenario: 设备身份错误不映射为 resolver 错误 | 1 |
| OP-15 | no | `design/features/06-open-recovery.feature` | 外层返回 RecoveryFailed 且 kind 为 LogStateIndeterminate 并保留 source | Feature: open、resolver 与恢复裁决 + Scenario: 日志状态不可判定返回 LogStateIndeterminate | 1 |
| OP-16 | no | `design/features/06-open-recovery.feature` | open 再次返回 NotInitialized 且没有残留临时对象 | Feature: open、resolver 与恢复裁决 + Scenario: NotInitialized 后临时容器清理幂等 | 1 |
| OP-17 | no | `design/features/06-open-recovery.feature` | 返回 OpenError::RecoveryFailed、保留 operation=<operation>、DeviceKey=<key> 和 source，释放此前取得的资源且不返回部分 Superblock | Feature: open、resolver 与恢复裁决 + Scenario Outline: open 设备阶段错误保留操作上下文且不返回部分实例 | 2 |
| SS-01 | yes | `design/features/07-snapshot-blockref.feature` | snapshot 固定包含 B 的读取视图 | Feature: snapshot、BlockRef 与读取语义 + Scenario: release 前创建的旧 snapshot 保留旧 State | 1 |
| SS-02 | no | `design/features/07-snapshot-blockref.feature` | 旧 snapshot 读取 B 返回原始完整数据 | Feature: snapshot、BlockRef 与读取语义 + Scenario: release 和 remove 后旧 snapshot 仍可读取 | 1 |
| SS-03 | no | `design/features/07-snapshot-blockref.feature` | 返回 SuperblockError::InvalidBlockRef 且 backend read 未调用 | Feature: snapshot、BlockRef 与读取语义 + Scenario: release 后新 snapshot 拒绝 BlockRef | 1 |
| SS-04 | no | `design/features/07-snapshot-blockref.feature` | 返回 SuperblockError::InvalidBlockRef 且 backend read 未调用 | Feature: snapshot、BlockRef 与读取语义 + Scenario: 未注册 DeviceKey 返回 SuperblockError | 1 |
| SS-05 | no | `design/features/07-snapshot-blockref.feature` | 返回 SuperblockError::InvalidBlockRef 且 backend read 未调用 | Feature: snapshot、BlockRef 与读取语义 + Scenario: 非法 BlockId 返回 SuperblockError | 1 |
| SS-06 | no | `design/features/07-snapshot-blockref.feature` | 返回 SuperblockError::InvalidBlockRef 且 backend read 未调用 | Feature: snapshot、BlockRef 与读取语义 + Scenario: 不属于固定 State 的 BlockRef 返回 SuperblockError | 1 |
| SS-07 | yes | `design/features/07-snapshot-blockref.feature` | 旧 snapshot 读取 B 返回完整数据 | Feature: snapshot、BlockRef 与读取语义 + Scenario: release 前旧 snapshot 读取 released block | 1 |
| SS-08 | no | `design/features/07-snapshot-blockref.feature` | 返回 SuperblockError::InvalidBlockRef 且 backend read 未调用 | Feature: snapshot、BlockRef 与读取语义 + Scenario: release 后新 snapshot 拒绝 released BlockRef | 1 |
| SS-09 | no | `design/features/07-snapshot-blockref.feature` | 返回 B 的完整数据且返回的 ReadBuf 在释放前阻止物理删除 | Feature: snapshot、BlockRef 与读取语义 + Scenario: Superblock read 观察当前 State | 1 |
| SS-10 | no | `design/features/07-snapshot-blockref.feature` | worker 等待 ReadBuf 释放后才完成删除 | Feature: snapshot、BlockRef 与读取语义 + Scenario: Superblock read 返回的 ReadBuf 保护对象 | 1 |
| SS-11 | no | `design/features/07-snapshot-blockref.feature` | 返回顺序为 1、2、3 | Feature: snapshot、BlockRef 与读取语义 + Scenario: devices 按 DeviceKey 升序返回 | 1 |
| SS-12 | no | `design/features/07-snapshot-blockref.feature` | 返回 None | Feature: snapshot、BlockRef 与读取语义 + Scenario: device_state 对未知 key 返回 None | 1 |
| SS-13 | no | `design/features/07-snapshot-blockref.feature` | 结果按 level 升序再按 SegmentVersion 升序返回 | Feature: snapshot、BlockRef 与读取语义 + Scenario: segment_infos 按 level 和 version 排序 | 1 |
| SS-14 | no | `design/features/07-snapshot-blockref.feature` | 返回 SuperblockError::InvalidBlockRef 且 backend read 未调用 | Feature: snapshot、BlockRef 与读取语义 + Scenario: 非法 SegmentId 返回 SuperblockError | 1 |
| SS-15 | no | `design/features/07-snapshot-blockref.feature` | BlockRef 按 slot/index 升序返回 | Feature: snapshot、BlockRef 与读取语义 + Scenario: live_blocks 按 slot 顺序返回 | 1 |
| SS-16 | no | `design/features/07-snapshot-blockref.feature` | 两个查询都返回空迭代器 | Feature: snapshot、BlockRef 与读取语义 + Scenario: 空 roots 和 bindings 返回空迭代器 | 1 |
| SS-17 | no | `design/features/07-snapshot-blockref.feature` | 按 BlockRef tuple 升序返回 RootVersion 值 | Feature: snapshot、BlockRef 与读取语义 + Scenario: roots 按 BlockRef 排序并按值返回 | 1 |
| SS-18 | no | `design/features/07-snapshot-blockref.feature` | 返回 Some(RootVersion { block: B, published_at: R }) 且结果为派生 Copy 值 | Feature: snapshot、BlockRef 与读取语义 + Scenario: get_root 命中时按值返回 published_at | 1 |
| SS-19 | no | `design/features/07-snapshot-blockref.feature` | 返回 None | Feature: snapshot、BlockRef 与读取语义 + Scenario: get_root 未命中时返回 None | 1 |
| SS-20 | no | `design/features/07-snapshot-blockref.feature` | 按原始名称 UTF-8 字节升序返回名称和 BlockRef | Feature: snapshot、BlockRef 与读取语义 + Scenario: bindings 按原始名称字节排序 | 1 |
| SS-21 | no | `design/features/07-snapshot-blockref.feature` | 返回 Some(B)、BlockRef 按值复制且不借用 map entry | Feature: snapshot、BlockRef 与读取语义 + Scenario: get_binding 按原始 UTF-8 字节精确命中并按值返回 | 1 |
| SS-22 | no | `design/features/07-snapshot-blockref.feature` | 返回 None | Feature: snapshot、BlockRef 与读取语义 + Scenario: get_binding 不执行大小写折叠 | 1 |
| SS-23 | no | `design/features/07-snapshot-blockref.feature` | 返回 None 且两个规范等价名称仍按不同原始字节键处理 | Feature: snapshot、BlockRef 与读取语义 + Scenario: get_binding 不执行 Unicode 规范化 | 1 |
| SS-24 | no | `design/features/07-snapshot-blockref.feature` | 返回 None | Feature: snapshot、BlockRef 与读取语义 + Scenario: get_binding 未命中返回 None | 1 |
| SS-25 | yes | `design/features/07-snapshot-blockref.feature` | 旧 snapshot 的 record_seq 仍为 R、application 仍为 A 且新 snapshot 返回 R+1 和 B | Feature: snapshot、BlockRef 与读取语义 + Scenario: snapshot record_seq 和 application 在新 commit 后保持固定 | 1 |
| SS-26 | no | `design/features/07-snapshot-blockref.feature` | acquire_snapshot_lease 调用顺序为 1、2、3 且返回完整 snapshot | Feature: snapshot、BlockRef 与读取语义 + Scenario: open_snapshot 按 DeviceKey 顺序获取全部 lease | 1 |
| SS-27 | no | `design/features/07-snapshot-blockref.feature` | 返回 SuperblockError::Device、operation=acquire_snapshot_lease、DeviceKey=<failed_key>、保留 source、此前 lease 各释放一次、后续 key 不请求且不返回部分 snapshot | Feature: snapshot、BlockRef 与读取语义 + Scenario Outline: open_snapshot 获取失败释放全部既有 lease | 3 |
| SS-28 | yes | `design/features/07-snapshot-blockref.feature` | snapshot 仍通过自身强 Arc<D> 和 lease 完成读取，直到 snapshot 与其 ReadBuf 全部释放前物理回收保持阻塞 | Feature: snapshot、BlockRef 与读取语义 + Scenario: snapshot 独立持有 backend 强引用和固定 State lease | 1 |
| SS-29 | no | `design/features/07-snapshot-blockref.feature` | 返回 SuperblockError::Device、operation=read、DeviceKey=<key>、保留 source 且不返回部分 ReadBuf | Feature: snapshot、BlockRef 与读取语义 + Scenario Outline: Superblock 和 snapshot read 映射 DeviceError | 2 |
| SS-30 | yes | `design/features/07-snapshot-blockref.feature` | 返回 SuperblockError::Device、operation=acquire_snapshot_lease、DeviceKey=3、保留 source、backend read 调用次数为零且不返回部分 ReadBuf | Feature: snapshot、BlockRef 与读取语义 + Scenario: Superblock read 临时 lease 失败保留完整上下文 | 1 |
| TX-01 | yes | `design/features/08-transaction-changes.feature` | 两条路径都按 A、X、B 顺序得到相同 candidate State | Feature: 事务顺序与受控变化 + Scenario: System A、Application X、System B 保持交错 | 1 |
| TX-02 | no | `design/features/08-transaction-changes.feature` | 公开 API 不提供该构造入口 | Feature: 事务顺序与受控变化 + Scenario: 应用不能直接 push 任意 SystemChange | 1 |
| TX-03 | no | `design/features/08-transaction-changes.feature` | candidate roots 增加该 BlockRef | Feature: 事务顺序与受控变化 + Scenario: AddRoot 成功登记 live BlockRef | 1 |
| TX-04 | no | `design/features/08-transaction-changes.feature` | 返回 SuperblockError::InvalidBlockRef 且 candidate 不发布且 backend 不调用 | Feature: 事务顺序与受控变化 + Scenario: 重复 AddRoot 返回 S3裁决 SYSTEM-CONFLICT=SuperblockError::InvalidBlockRef 决策错误 | 1 |
| TX-05 | no | `design/features/08-transaction-changes.feature` | 两个 change 均成功且最终 roots 不包含 B | Feature: 事务顺序与受控变化 + Scenario: RemoveRoot 后释放同一 block 成功 | 1 |
| TX-06 | no | `design/features/08-transaction-changes.feature` | 返回 SuperblockError::InvalidBlockRef 且 candidate 不发布且 backend 不调用 | Feature: 事务顺序与受控变化 + Scenario: 释放后 AddRoot 返回 S3裁决 SYSTEM-CONFLICT=SuperblockError::InvalidBlockRef 决策错误 | 1 |
| TX-07 | no | `design/features/08-transaction-changes.feature` | 返回 SuperblockError::InvalidBlockRef 且 candidate 不发布且 backend 不调用 | Feature: 事务顺序与受控变化 + Scenario: AddRoot 非 live block 返回 S3裁决 SYSTEM-CONFLICT=SuperblockError::InvalidBlockRef 决策错误 | 1 |
| TX-08 | no | `design/features/08-transaction-changes.feature` | 最终 main 绑定 B2 | Feature: 事务顺序与受控变化 + Scenario: 同名 binding 删除后重建成功 | 1 |
| TX-09 | no | `design/features/08-transaction-changes.feature` | 返回 SuperblockError::InvalidBlockRef 且 candidate 不发布且 backend 不调用 | Feature: 事务顺序与受控变化 + Scenario: 重复 binding 返回 S3裁决 SYSTEM-CONFLICT=SuperblockError::InvalidBlockRef 决策错误 | 1 |
| TX-10 | no | `design/features/08-transaction-changes.feature` | 返回 SuperblockError::InvalidBlockRef 且 candidate 不发布且 backend 不调用 | Feature: 事务顺序与受控变化 + Scenario: 重复 release 返回 S3裁决 SYSTEM-CONFLICT=SuperblockError::InvalidBlockRef 决策错误 | 1 |
| TX-11 | no | `design/features/08-transaction-changes.feature` | 返回 SuperblockError::InvalidBlockRef 且 candidate 不发布且 backend 不调用 | Feature: 事务顺序与受控变化 + Scenario: release 未分配 block 返回 S3裁决 SYSTEM-CONFLICT=SuperblockError::InvalidBlockRef 决策错误 | 1 |
| TX-12 | no | `design/features/08-transaction-changes.feature` | candidate 追加新的虚拟 Active 且旧 Active 变为 Sealed | Feature: 事务顺序与受控变化 + Scenario: CreateActiveSegment 合法变更成功 | 1 |
| TX-13 | no | `design/features/08-transaction-changes.feature` | candidate 追加非空 AllocationRun | Feature: 事务顺序与受控变化 + Scenario: AllocateSegmentBlocks 合法变更成功 | 1 |
| TX-14 | no | `design/features/08-transaction-changes.feature` | candidate 为目标槽位写入当前 RecordSeq | Feature: 事务顺序与受控变化 + Scenario: ReleaseSegmentBlocks 合法变更成功 | 1 |
| TX-15 | no | `design/features/08-transaction-changes.feature` | candidate 移除该 segment 并产生 RetiredObject::Segment | Feature: 事务顺序与受控变化 + Scenario: RemoveSegment 合法变更成功 | 1 |
| TX-16 | no | `design/features/08-transaction-changes.feature` | candidate 插入 LargeBlockData 并推进 next_id | Feature: 事务顺序与受控变化 + Scenario: AllocateLargeBlock 合法变更成功 | 1 |
| TX-17 | no | `design/features/08-transaction-changes.feature` | candidate 写入当前 RecordSeq 的 released_at | Feature: 事务顺序与受控变化 + Scenario: ReleaseLargeBlock 合法变更成功 | 1 |
| TX-18 | no | `design/features/08-transaction-changes.feature` | candidate 移除 entry 并产生 RetiredObject::LargeBlock | Feature: 事务顺序与受控变化 + Scenario: RemoveLargeBlock 合法变更成功 | 1 |
| TX-19 | no | `design/features/08-transaction-changes.feature` | 返回 SuperblockError::InvalidBlockRef 且 candidate 不发布且 backend 不调用 | Feature: 事务顺序与受控变化 + Scenario: 受控 SystemChange 冲突返回 S3裁决 SYSTEM-CONFLICT=SuperblockError::InvalidBlockRef 决策错误 | 1 |
| TX-20 | yes | `design/features/08-transaction-changes.feature` | 一个事务先取得 gate 且另一个事务等待其释放 | Feature: 事务顺序与受控变化 + Scenario: single writer gate 串行化持久修改 | 1 |
| TX-21 | no | `design/features/08-transaction-changes.feature` | 所有 session 已终结且 writer gate 可被下一事务取得 | Feature: 事务顺序与受控变化 + Scenario: commit 失败后 session 和 gate 可观察释放 | 1 |
| TX-22 | no | `design/features/08-transaction-changes.feature` | flush 顺序为 1、2、3 | Feature: 事务顺序与受控变化 + Scenario: 多设备 flush 按 DeviceKey 升序 | 1 |
| TX-23 | yes | `design/features/08-transaction-changes.feature` | metadata log 只追加一个按顺序编码的 Delta | Feature: 事务顺序与受控变化 + Scenario: 多设备事务只追加一个 Delta | 1 |
| TX-24 | no | `design/features/08-transaction-changes.feature` | 恢复后的 State 与提交时发布的 State 相同 | Feature: 事务顺序与受控变化 + Scenario: 恢复重建跨设备 State | 1 |
| TX-25 | yes | `design/features/08-transaction-changes.feature` | logical change 顺序仅为 System A、Application B、System C；batch request、write_at、chunk、flush、participant 和物理调度不进入 TransactionChange、ApplicationApplyContext 或 wire | Feature: 事务顺序与受控变化 + Scenario: write_batch 与逻辑 changes 保持严格交错边界 | 1 |
| TX-26 | no | `design/features/08-transaction-changes.feature` | 返回 <result> | Feature: 事务顺序与受控变化 + Scenario Outline: BindingName 公共构造精确校验 UTF-8 字节长度 | 5 |
| TX-27 | no | `design/features/08-transaction-changes.feature` | 构造返回 SuperblockError::InvalidBindingName、transaction 方法未调用、不增加 SystemChange、不调用 backend I/O 且无 append | Feature: 事务顺序与受控变化 + Scenario Outline: 非法 BindingName 在 transaction 调用前零副作用失败 | 6 |
| TX-28 | no | `design/features/08-transaction-changes.feature` | 生成一个保持原始名称字节的受控 binding change | Feature: 事务顺序与受控变化 + Scenario Outline: bind 和 unbind 接受 BindingName 合法边界 | 4 |
| TX-29 | yes | `design/features/08-transaction-changes.feature` | 三次调用收到完全相同的 opaque TransactionId | Feature: 事务顺序与受控变化 + Scenario: 同一事务的所有 participant 使用相同 TransactionId | 1 |
| TX-30 | no | `design/features/08-transaction-changes.feature` | 不同 live transaction 的 TransactionId 两两不同 | Feature: 事务顺序与受控变化 + Scenario: 并发 live transaction 的 TransactionId 唯一 | 1 |
| TX-31 | no | `design/features/08-transaction-changes.feature` | 均不持久化 TransactionId 且它不等于或替代 RecordSeq | Feature: 事务顺序与受控变化 + Scenario: TransactionId 仅为进程内临时标识 | 1 |
| TX-32 | no | `design/features/08-transaction-changes.feature` | 同一进程生命周期内已分配值永不复用，重启后允许重复且 live transaction 唯一性始终保持 | Feature: 事务顺序与受控变化 + Scenario: TransactionId 结束后复用由 S3裁决 TRANSACTION-ID-REUSE=同进程已分配值永不复用且重启后可重复 决定 | 1 |
| TX-33 | no | `design/features/08-transaction-changes.feature` | 仍按批准签名返回 &S，且 S3裁决 TRANSACTION-APPLICATION-CLOSED=纯查询在closed或fenced后可用 明确其不适用 TransactionClosed 或 RecoveryRequired 命令错误优先规则 | Feature: 事务顺序与受控变化 + Scenario: closed 或围栏后 application 纯查询保持可用 | 1 |
| TX-34 | yes | `design/features/08-transaction-changes.feature` | 返回 <error> 且不产生 change、I/O 或 append | Feature: 事务顺序与受控变化 + Scenario Outline: closed transaction 命令方法返回稳定状态错误 | 22 |
| TX-35 | yes | `design/features/08-transaction-changes.feature` | 每个 participant 恰好执行一次 <terminal> 且不执行其他 session 终态、transaction closed、writer gate 在全部阶段清理完成后 released、published State 为 <published_state>、RecordSeq 为 <record_seq>、retirement responsibility 为 <retirement>、SuperblockStatus 为 <status> 且下一持久修改为 <next_writer> | Feature: 事务顺序与受控变化 + Scenario Outline: Transaction 普通 Drop 按提交阶段终结 participant | 3 |
| TX-36 | no | `design/features/08-transaction-changes.feature` | 记录 operation=<action>、DeviceKey=<failed_key> 和 source=E、SuperblockStatus=RecoveryRequired、其余 participant 仍各消费一次阶段对应终态、失败 participant 不尝试第二种 session 终态、transaction closed、writer gate released、已知 durable State 和 RecordSeq 不撤销，且 enqueue_retirements 失败时保留由 reopen 补偿的 retirement responsibility | Feature: 事务顺序与受控变化 + Scenario Outline: Transaction 普通 Drop 的 participant 清理失败仍完成全局清理 | 4 |
| TX-37 | no | `design/features/08-transaction-changes.feature` | 记录 operation=<action>、DeviceKey=<failed_key> 和 panic 诊断、SuperblockStatus=RecoveryRequired、同一 participant 不尝试其他 session 终态、writer gate 最终 released、已知 durable State 和 RecordSeq 不撤销且 panic 继续向调用方传播 | Feature: 事务顺序与受控变化 + Scenario Outline: Transaction 普通 Drop 的 participant 清理 panic 围栏后继续 unwind | 4 |
| TX-38 | yes | `design/features/08-transaction-changes.feature` | 按已冻结优先级 RecoveryRequired 高于 TransactionClosed、TransactionClosed 高于 ReadOnlyExhausted 返回 <error>，且不产生 TransactionChange、不修改 candidate 或 published State、不建立新 participant、backend begin_write、write_at 和 flush 均未调用、RecordSeq 不分配且 metadata append、publish 和 enqueue_retirements 均未调用 | Feature: 事务顺序与受控变化 + Scenario Outline: 已有 Transaction 在 ReadOnlyExhausted 下按状态优先级拒绝命令 | 14 |
| BW-01 | no | `design/features/09-bytes-stream.feature` | 返回 StreamWriteError::Superblock(SuperblockError::InvalidBlockRef)、不产生 allocation 或 write_at 且同一事务可继续使用 | Feature: Bytes 与 stream 精确长度 + Scenario: logical_len 为零拒绝且事务保持可用 | 1 |
| BW-02 | no | `design/features/09-bytes-stream.feature` | 结果为 <result> 且长度不匹配时 backend 不调用 | Feature: Bytes 与 stream 精确长度 + Scenario Outline: BlockWrite 构造校验精确长度 | 3 |
| BW-03 | no | `design/features/09-bytes-stream.feature` | 凭证归 BlockWrite 所有且调用方不能再次使用该凭证 | Feature: Bytes 与 stream 精确长度 + Scenario: BlockWrite 构造成功消费线性凭证 | 1 |
| BW-04 | yes | `design/features/09-bytes-stream.feature` | 编译失败且只有当前 transaction 产生的 AllocatedBlock 才能授权写入 | Feature: Bytes 与 stream 精确长度 + Scenario Outline: 裸 BlockRef 不授予写权限 | 3 |
| BW-05 | yes | `design/features/09-bytes-stream.feature` | 使用普通 level 14 并返回最终 BlockRef | Feature: Bytes 与 stream 精确长度 + Scenario: 512 KiB Bytes 写入成功 | 1 |
| BW-06 | no | `design/features/09-bytes-stream.feature` | 使用 independent large block 且写入精确长度 | Feature: Bytes 与 stream 精确长度 + Scenario: 512 KiB 加一字节使用独立大块 | 1 |
| BW-07 | no | `design/features/09-bytes-stream.feature` | 返回 StreamTooShort、全部 session abandon、transaction closed 且 gate released | Feature: Bytes 与 stream 精确长度 + Scenario: stream 少一个字节关闭事务 | 1 |
| BW-08 | no | `design/features/09-bytes-stream.feature` | StreamTooLong 报告 written=N 和 logical_len=N、该 chunk 零字节写入、全部 session abandon、transaction closed 且 gate released | Feature: Bytes 与 stream 精确长度 + Scenario: stream 多一个字节在写前关闭事务 | 1 |
| BW-09 | no | `design/features/09-bytes-stream.feature` | 返回 StreamTooLong 且额外 chunk 不调用 write_at | Feature: Bytes 与 stream 精确长度 + Scenario: 达到 N 后额外非空 chunk 被拒绝 | 1 |
| BW-10 | no | `design/features/09-bytes-stream.feature` | written 不增加且仍等待后续 chunk 或 EOF | Feature: Bytes 与 stream 精确长度 + Scenario: 空 chunk 不表示完成 | 1 |
| BW-11 | no | `design/features/09-bytes-stream.feature` | <result> | Feature: Bytes 与 stream 精确长度 + Scenario Outline: 多 chunk stream 精确写满或拒绝非法 checked position | 4 |
| BW-12 | no | `design/features/09-bytes-stream.feature` | 返回携带 E 的 stream item error、全部 session abandon、transaction closed 且 gate released | Feature: Bytes 与 stream 精确长度 + Scenario: stream item error 关闭整个事务 | 1 |
| BW-13 | yes | `design/features/09-bytes-stream.feature` | 全部 session abandon、transaction closed 且 gate released | Feature: Bytes 与 stream 精确长度 + Scenario: Stream next await 取消立即 abandon | 1 |
| BW-14 | no | `design/features/09-bytes-stream.feature` | 全部 session abandon、transaction closed 且 gate released | Feature: Bytes 与 stream 精确长度 + Scenario: backend write_at await 取消立即 abandon | 1 |
| BW-15 | no | `design/features/09-bytes-stream.feature` | 全部 session abandon、transaction closed 且 gate released | Feature: Bytes 与 stream 精确长度 + Scenario: commit flush await 取消立即 abandon | 1 |
| BW-16 | no | `design/features/09-bytes-stream.feature` | transaction closed、gate released 且 SuperblockStatus=RecoveryRequired | Feature: Bytes 与 stream 精确长度 + Scenario: abandon 失败进入 RecoveryRequired | 1 |
| BW-17 | no | `design/features/09-bytes-stream.feature` | 编译失败且 application codec 不接受 Stream | Feature: Bytes 与 stream 精确长度 + Scenario: Application codec 不接受 stream 输入 | 1 |
| BW-18 | yes | `design/features/09-bytes-stream.feature` | 返回使用普通 <level> 的线性 AllocatedBlock | Feature: Bytes 与 stream 精确长度 + Scenario Outline: alloc_on 为十五个普通 level 选择最小可容纳级别 | 30 |
| BW-19 | no | `design/features/09-bytes-stream.feature` | 返回 independent large block 的线性 AllocatedBlock | Feature: Bytes 与 stream 精确长度 + Scenario: alloc_on 超过 level 14 上界时选择独立大块 | 1 |
| BW-20 | no | `design/features/09-bytes-stream.feature` | 返回 SuperblockError::InvalidBlockRef、不产生 allocation change、session 或 backend I/O | Feature: Bytes 与 stream 精确长度 + Scenario Outline: alloc_on 拒绝非法目标或耗尽 | 4 |
| BW-21 | no | `design/features/09-bytes-stream.feature` | 每个请求恰好完成一次 write_at、对应 participant 可继续 flush 且所有凭证标记为完整写入 | Feature: Bytes 与 stream 精确长度 + Scenario: write_batch 多请求全部成功 | 1 |
| BW-22 | no | `design/features/09-bytes-stream.feature` | 返回 Result<(),SuperblockError> 的 SuperblockError::InvalidBlockRef、所有 backend begin_write 和 write_at 调用次数均为零且 transaction 保持可用 | Feature: Bytes 与 stream 精确长度 + Scenario: write_batch 其他 transaction 合法凭证预检失败时零 I/O | 1 |
| BW-23 | no | `design/features/09-bytes-stream.feature` | 返回构造错误、无法形成 BlockWrite、backend I/O 和 metadata append 均不调用 | Feature: Bytes 与 stream 精确长度 + Scenario Outline: BlockWrite 构造期长度不匹配拒绝且不可进入 write_batch | 2 |
| BW-24 | yes | `design/features/09-bytes-stream.feature` | 编译失败且运行时整批预检不存在重复消费同一 allocation 的输入 | Feature: Bytes 与 stream 精确长度 + Scenario: write_batch 重复同一 AllocatedBlock 在类型系统中不可达 | 1 |
| BW-25 | yes | `design/features/09-bytes-stream.feature` | 返回 SuperblockError::Device、operation=write、对应 DeviceKey 和 source、全部 session abandon、transaction closed、gate released 且不返回部分成功结果 | Feature: Bytes 与 stream 精确长度 + Scenario: write_batch 中途 DeviceError 关闭事务 | 1 |
| BW-26 | no | `design/features/09-bytes-stream.feature` | 先分配下一 RecordSeq但在 physical-written 和凭证纯预检失败时不发布或持久化该序号、返回 CommitFailed、published State 不变、全部 session abandon、transaction closed、gate released、后续命令返回 TransactionClosed、下一事务可取得 gate 并可重新使用同一 candidate 序号 | Feature: Bytes 与 stream 精确长度 + Scenario Outline: commit 在 append 前拒绝未完整写凭证 | 2 |
| BW-27 | yes | `design/features/09-bytes-stream.feature` | key1 session 恰好 abandon 一次、key2 不调用任何 session 终态且其局部 RAII 资源全部释放、未尝试后续 DeviceKey、transaction closed、gate released、candidate 和 published State 不变、RecordSeq 未分配或消费、metadata append、publish 和 enqueue_retirements 均未调用且下一 transaction 可取得 gate | Feature: Bytes 与 stream 精确长度 + Scenario: key1 session 已存在时等待 key2 begin_write 取消关闭外层事务 | 1 |
| BW-28 | yes | `design/features/09-bytes-stream.feature` | 返回 SuperblockError::Device、operation=begin_write、DeviceKey=2、source=E，key1 session 恰好 abandon 一次、key2 不产生可由外层终结的 session 且局部 RAII 资源全部释放、未尝试后续 DeviceKey、transaction closed、gate released、candidate 和 published State 不变、RecordSeq、metadata append、publish 和 retirement 登记均无副作用且不返回部分成功结果 | Feature: Bytes 与 stream 精确长度 + Scenario: key1 session 已存在时 key2 begin_write DeviceError 关闭外层事务 | 1 |
| AP-01 | no | `design/features/10-application.feature` | 结果为 <result> | Feature: Application state、codec 与 context + Scenario Outline: State 编码大小边界 | 3 |
| AP-02 | no | `design/features/10-application.feature` | 结果为 <result> | Feature: Application state、codec 与 context + Scenario Outline: Delta 编码大小边界 | 3 |
| AP-03 | no | `design/features/10-application.feature` | metadata 由 application payload 自己编码、识别和校验，payload roundtrip 保留 metadata，且不进入通用 superblock header | Feature: Application state、codec 与 context + Scenario: application payload 自己解释 format metadata | 1 |
| AP-04 | no | `design/features/10-application.feature` | 返回 OpenError::RecoveryFailed、kind=RecoveryFailureKind::Decode 且保留 codec source | Feature: Application state、codec 与 context + Scenario: open 映射 Delta codec mismatch | 1 |
| AP-05 | no | `design/features/10-application.feature` | 返回 ApplicationCodecError 且不产生 ApplicationState | Feature: Application state、codec 与 context + Scenario: Empty codec 单元拒绝非空 state | 1 |
| AP-06 | no | `design/features/10-application.feature` | 返回 CreateError::NotCreated 且 kind=ApplicationCodec 并保留 source | Feature: Application state、codec 与 context + Scenario: create 映射 Empty codec 非空 state | 1 |
| AP-07 | no | `design/features/10-application.feature` | 返回 OpenError::RecoveryFailed 且 kind=Decode 并保留 source | Feature: Application state、codec 与 context + Scenario: open 映射 Empty codec 非空 state | 1 |
| AP-08 | no | `design/features/10-application.feature` | context 包含 A 的逻辑结果且不包含 B、physical write 或 flush | Feature: Application state、codec 与 context + Scenario: ApplicationApplyContext 只包含此前逻辑变化 | 1 |
| AP-09 | yes | `design/features/10-application.feature` | 超级块以 BTreeSet 去重并各验证一次 B1 和 B2 | Feature: Application state、codec 与 context + Scenario: visitor 重复引用去重后验证全部持久 BlockRef | 1 |
| AP-10 | no | `design/features/10-application.feature` | 返回 SuperblockError::InvalidBlockRef、candidate 不发布且 backend write 和 flush 不调用 | Feature: Application state、codec 与 context + Scenario: visitor 非 live 引用阻止 commit | 1 |
| AP-11 | no | `design/features/10-application.feature` | 不生成 ReleaseSegmentBlocks、ReleaseLargeBlock、RemoveSegment 或 RemoveLargeBlock | Feature: Application state、codec 与 context + Scenario: visitor 不触发 GC 或自动 release | 1 |
| AP-12 | no | `design/features/10-application.feature` | 返回 ApplicationApplyError 且已发布 State 不变 | Feature: Application state、codec 与 context + Scenario: application apply 失败不发布 candidate | 1 |
| AP-13 | no | `design/features/10-application.feature` | 外层 transaction API 返回成功且 candidate 可继续校验 | Feature: Application state、codec 与 context + Scenario: visitor 成功完成外层校验 | 1 |
| AP-14 | no | `design/features/10-application.feature` | 外层返回 SuperblockError 且 kind=ApplicationVisit 并保留 source | Feature: Application state、codec 与 context + Scenario: visitor 错误映射 ApplicationVisit | 1 |
| AP-15 | yes | `design/features/10-application.feature` | 编译失败且底层 BytesMut 保持私有 | Feature: Application state、codec 与 context + Scenario Outline: ApplicationEncoder 不暴露可绕过 hard_limit 的可变入口 | 4 |
| AP-16 | no | `design/features/10-application.feature` | 返回 ApplicationCodecError、buffer 长度和原有 bytes 完全不变且 remaining 不变 | Feature: Application state、codec 与 context + Scenario Outline: ApplicationEncoder 每个 put 越界前原子失败 | 5 |
| AP-17 | no | `design/features/10-application.feature` | 每次成功后 remaining 按实际写入字节减少 <bytes>、buffer 只包含已写入 bytes 且没有容量垃圾 | Feature: Application state、codec 与 context + Scenario Outline: ApplicationEncoder 成功 put 按实际字节递减 remaining | 5 |
| AP-18 | yes | `design/features/10-application.feature` | 只返回精确的 payload P、不包含容量垃圾、encoder 被消费且不再拥有可写状态 | Feature: Application state、codec 与 context + Scenario: ApplicationEncoder freeze 返回已写 bytes 并消费 self | 1 |
| AP-19 | no | `design/features/10-application.feature` | 编译失败 | Feature: Application state、codec 与 context + Scenario: ApplicationEncoder freeze 后不能再次使用 | 1 |
| AP-20 | n/a | `design/features/10-application.feature:2` | 当前 feature 未提供可对应的 Scenario/Scenario Outline | Feature: Application state、codec 与 context + 未映射 | 0 |
| AP-21 | n/a | `design/features/10-application.feature:2` | 当前 feature 未提供可对应的 Scenario/Scenario Outline | Feature: Application state、codec 与 context + 未映射 | 0 |
| AP-22 | n/a | `design/features/10-application.feature:2` | 当前 feature 未提供可对应的 Scenario/Scenario Outline | Feature: Application state、codec 与 context + 未映射 | 0 |
| AP-23 | n/a | `design/features/10-application.feature:2` | 当前 feature 未提供可对应的 Scenario/Scenario Outline | Feature: Application state、codec 与 context + 未映射 | 0 |
| AP-24 | n/a | `design/features/10-application.feature:2` | 当前 feature 未提供可对应的 Scenario/Scenario Outline | Feature: Application state、codec 与 context + 未映射 | 0 |
| AP-25 | n/a | `design/features/10-application.feature:2` | 当前 feature 未提供可对应的 Scenario/Scenario Outline | Feature: Application state、codec 与 context + 未映射 | 0 |
| CO-01 | yes | `design/features/11-commit-outcomes.feature` | 返回 CommitFailed、candidate 和 published State 不变、RecordSeq 未消费、全部 session abandon 且下一事务可取得 gate | Feature: commit、create 三态与公开状态 + Scenario: append NotAppended 完整回滚未发布事务 | 1 |
| CO-02 | yes | `design/features/11-commit-outcomes.feature` | 返回 Ok(CommitResult { record_seq: R, status: Ready }) | Feature: commit、create 三态与公开状态 + Scenario: append Durable 普通序号返回 Ready | 1 |
| CO-03 | no | `design/features/11-commit-outcomes.feature` | 返回 Ok(CommitResult { record_seq: u32::MAX-1, status: ReadOnlyExhausted }) | Feature: commit、create 三态与公开状态 + Scenario: append Durable 最后合法序号返回 ReadOnlyExhausted | 1 |
| CO-04 | no | `design/features/11-commit-outcomes.feature` | 返回 CommitStatusUnknown、全部 session preserve、零 session abandon、transaction closed、gate released、status=RecoveryRequired 且必须 open | Feature: commit、create 三态与公开状态 + Scenario: append Unknown 保留 participant 并围栏 | 1 |
| CO-05 | no | `design/features/11-commit-outcomes.feature` | 不产生 DurableAppendOutcome、全部 session preserve、零 session abandon、transaction closed、gate released、status=RecoveryRequired 且必须 open | Feature: commit、create 三态与公开状态 + Scenario: append future cancellation 没有 DurableAppendOutcome | 1 |
| CO-06 | no | `design/features/11-commit-outcomes.feature` | status 保持 RecoveryRequired 且其他 participant 仍各消费一次 preserve | Feature: commit、create 三态与公开状态 + Scenario: append cancellation 中 preserve 失败仍保持 RecoveryRequired | 1 |
| CO-07 | no | `design/features/11-commit-outcomes.feature` | 返回 Ok(CommitResult) 且 status=RecoveryRequired | Feature: commit、create 三态与公开状态 + Scenario Outline: durable 后单个 participant 故障返回成功事实 | 3 |
| CO-08 | no | `design/features/11-commit-outcomes.feature` | RecordSeq 保持已消费且 status=RecoveryRequired | Feature: commit、create 三态与公开状态 + Scenario: publish failure 不撤销 durable 业务结果 | 1 |
| CO-09 | no | `design/features/11-commit-outcomes.feature` | 诊断记录 finish 和 DeviceKey 2 且 CommitResult 仅含 record_seq 与 status | Feature: commit、create 三态与公开状态 + Scenario: finish failure 记录 participant 但不扩展 CommitResult | 1 |
| CO-10 | no | `design/features/11-commit-outcomes.feature` | commit status=RecoveryRequired 且 open 重新登记 retirement | Feature: commit、create 三态与公开状态 + Scenario: enqueue failure 交给 reopen 补偿 | 1 |
| CO-11 | yes | `design/features/11-commit-outcomes.feature` | phase 顺序为 prevalidate、按 key flush、单次 append、publish、按 key finish、按 key enqueue_retirements | Feature: commit、create 三态与公开状态 + Scenario: commit phase 顺序可观察 | 1 |
| CO-12 | no | `design/features/11-commit-outcomes.feature` | 结果只能是 Ready、ReadOnlyExhausted 或 RecoveryRequired | Feature: commit、create 三态与公开状态 + Scenario: SuperblockStatus 名称固定 | 1 |
| RS-01 | yes | `design/features/12-recordseq-maintenance.feature` | commit 返回成功且 SuperblockStatus 立即为 ReadOnlyExhausted | Feature: RecordSeq 与日志维护 + Scenario: MAX-2 发布 MAX-1 后立即只读耗尽 | 1 |
| RS-02 | yes | `design/features/12-recordseq-maintenance.feature` | 精确返回 SuperblockError::ReadOnlyExhausted、writer gate 不被取得或等待、不创建 Transaction 或 participant、backend begin_write、write_at 和 flush 均未调用、candidate 和 published State 不变、RecordSeq 不分配或消费、metadata append 未调用且 maintenance phase、archive retry queue 和 retirement queue 不变 | Feature: RecordSeq 与日志维护 + Scenario: ReadOnlyExhausted 拒绝公开 Superblock::begin_write 且零副作用 | 1 |
| RS-28 | no | `design/features/12-recordseq-maintenance.feature` | 返回内部复用的 SuperblockError::ReadOnlyExhausted、不取得 writer gate、不改变 maintenance phase、archive retry queue 或 retirement queue、不分配 RecordSeq、不调用 metadata append 且不产生任何日志或介质元数据副作用 | Feature: RecordSeq 与日志维护 + Scenario Outline: ReadOnlyExhausted 拒绝内部 log maintenance 触发且零副作用 | 3 |
| RS-03 | no | `design/features/12-recordseq-maintenance.feature` | published status 为 ReadOnlyExhausted 且不再执行 archive retry | Feature: RecordSeq 与日志维护 + Scenario: checkpoint 消费最后合法序号立即只读耗尽 | 1 |
| RS-04 | no | `design/features/12-recordseq-maintenance.feature` | open 返回 ReadOnlyExhausted 状态且 read 和 snapshot 仍可用 | Feature: RecordSeq 与日志维护 + Scenario: ReadOnlyExhausted 重启后保持 | 1 |
| RS-05 | no | `design/features/12-recordseq-maintenance.feature` | phase 从 Active 转为 RotateRequested | Feature: RecordSeq 与日志维护 + Scenario: Active 转换为 RotateRequested | 1 |
| RS-06 | no | `design/features/12-recordseq-maintenance.feature` | phase 转为 Rotating | Feature: RecordSeq 与日志维护 + Scenario: RotateRequested 转换为 Rotating | 1 |
| RS-07 | no | `design/features/12-recordseq-maintenance.feature` | phase 转回 Active 且不分配 RecordSeq | Feature: RecordSeq 与日志维护 + Scenario: rotate no-op 返回 Active | 1 |
| RS-08 | no | `design/features/12-recordseq-maintenance.feature` | phase 转为 CheckpointRequired | Feature: RecordSeq 与日志维护 + Scenario: rotate 成功转换为 CheckpointRequired | 1 |
| RS-09 | yes | `design/features/12-recordseq-maintenance.feature` | 事件严格为 clone 最新完整 State 为 candidate、设置 candidate.record_seq=R+1、编码含 registry、全部 device states、roots、bindings 和 application 的完整 checkpoint、rotate 创建新 Active、以 R+1 将 checkpoint 作为新 Active 首记录 durable、原子 publish candidate、释放 writer gate、进入 ArchivePending 后 archive 旧 structure | Feature: RecordSeq 与日志维护 + Scenario: maintenance checkpoint 完整前半与发布归档顺序 | 1 |
| RS-10 | yes | `design/features/12-recordseq-maintenance.feature` | 事件严格为 durable R+1、原子 publish、writer gate release、进入 ArchivePending 后 archive | Feature: RecordSeq 与日志维护 + Scenario: 非耗尽 checkpoint 事件顺序为 durable、publish、gate release、archive | 1 |
| RS-11 | no | `design/features/12-recordseq-maintenance.feature` | 分配 R+2 | Feature: RecordSeq 与日志维护 + Scenario: checkpoint 发布后下一业务使用 R+2 | 1 |
| RS-12 | no | `design/features/12-recordseq-maintenance.feature` | 返回 OpenError::RecoveryFailed、kind=RecoveryFailureKind::Decode 且不发布 Superblock | Feature: RecordSeq 与日志维护 + Scenario: 新 Active 首条 Delta 导致 RecoveryFailed | 1 |
| RS-13 | no | `design/features/12-recordseq-maintenance.feature` | phase 转为 Active | Feature: RecordSeq 与日志维护 + Scenario: ArchivePending 当前旧 structure 成功后返回 Active | 1 |
| RS-14 | no | `design/features/12-recordseq-maintenance.feature` | 旧 structure 加入 retry queue 且 phase 转为 ArchiveRetry | Feature: RecordSeq 与日志维护 + Scenario: ArchivePending 当前旧 structure 失败后进入 ArchiveRetry | 1 |
| RS-15 | no | `design/features/12-recordseq-maintenance.feature` | queue 保留未完成项且 phase 保持 ArchiveRetry | Feature: RecordSeq 与日志维护 + Scenario: ArchiveRetry 失败时保持 ArchiveRetry | 1 |
| RS-16 | no | `design/features/12-recordseq-maintenance.feature` | retry queue 为空且 phase 转为 Active | Feature: RecordSeq 与日志维护 + Scenario: ArchiveRetry 成功且队列清空后返回 Active | 1 |
| RS-17 | no | `design/features/12-recordseq-maintenance.feature` | retry queue 仍非空且 phase 保持 ArchiveRetry | Feature: RecordSeq 与日志维护 + Scenario: ArchiveRetry 成功但队列非空时保持 ArchiveRetry | 1 |
| RS-18 | no | `design/features/12-recordseq-maintenance.feature` | phase 保持 Active 且不追加维护记录 | Feature: RecordSeq 与日志维护 + Scenario: no-op maintenance 保持 Active | 1 |
| RS-19 | yes | `design/features/12-recordseq-maintenance.feature` | record_seq 不变、禁止继续 append、SuperblockStatus=RecoveryRequired 且调用方必须 open 裁决介质状态 | Feature: RecordSeq 与日志维护 + Scenario: rotate error 仅建立恢复围栏 | 1 |
| RS-20 | no | `design/features/12-recordseq-maintenance.feature` | record_seq 不发布、禁止继续 append、SuperblockStatus=RecoveryRequired 且调用方必须 open 裁决介质状态 | Feature: RecordSeq 与日志维护 + Scenario: rotate unknown 建立 RecoveryRequired | 1 |
| RS-21 | no | `design/features/12-recordseq-maintenance.feature` | open 从旧 structure 恢复原 record_seq 并返回 Ready | Feature: RecordSeq 与日志维护 + Scenario Outline: rotate 失败后介质未变可由 open 恢复 | 2 |
| RS-22 | no | `design/features/12-recordseq-maintenance.feature` | 返回 OpenError::RecoveryFailed、kind=LogStateIndeterminate 且不返回 Superblock | Feature: RecordSeq 与日志维护 + Scenario Outline: rotate 失败后介质状态不确定由 open 拒绝 | 2 |
| RS-23 | no | `design/features/12-recordseq-maintenance.feature` | record_seq 不变、新 Active 不可追加、SuperblockStatus=RecoveryRequired 且 open 可从旧完整 structure 恢复 | Feature: RecordSeq 与日志维护 + Scenario: rotate 后 checkpoint NotAppended 建立 RecoveryRequired | 1 |
| RS-24 | yes | `design/features/12-recordseq-maintenance.feature` | record_seq 不发布、新 Active 不可追加、SuperblockStatus=RecoveryRequired 且必须 open 裁决日志 | Feature: RecordSeq 与日志维护 + Scenario: rotate 后 checkpoint Unknown 建立 RecoveryRequired | 1 |
| RS-25 | no | `design/features/12-recordseq-maintenance.feature` | 不产生 DurableAppendOutcome、R+1 不发布、SuperblockStatus=RecoveryRequired 且必须 open 裁决 | Feature: RecordSeq 与日志维护 + Scenario: rotate 后 checkpoint cancellation 建立 RecoveryRequired | 1 |
| RS-26 | no | `design/features/12-recordseq-maintenance.feature` | 事件严格为 publish ReadOnlyExhausted、writer gate release、进入 ArchivePending 后首次 archive；首次 archive 失败时保留 retry responsibility 但 ReadOnlyExhausted 不执行 archive retry | Feature: RecordSeq 与日志维护 + Scenario: 最后合法 checkpoint 发布后进入首次 archive | 1 |
| RS-27 | no | `design/features/12-recordseq-maintenance.feature` | R+1 保持已消费且 SuperblockStatus 为 RecoveryRequired | Feature: RecordSeq 与日志维护 + Scenario: checkpoint durable 后 publish failure 建立 RecoveryRequired | 1 |
| WF-01 | yes | `design/features/13-wire-format.feature` | magic、system_version、record_kind、flags 位于第15.2节规定偏移且总长为 8 字节 | Feature: wire 格式与 golden 校验 + Scenario: PISB header 四字段位于冻结偏移 | 1 |
| WF-02 | no | `design/features/13-wire-format.feature` | flags 字段为 0 | Feature: wire 格式与 golden 校验 + Scenario: header flags 固定为零 | 1 |
| WF-03 | no | `design/features/13-wire-format.feature` | 返回 InternalValidationKind::InvalidState 且不发布 State | Feature: wire 格式与 golden 校验 + Scenario: 非零 header flags 映射 InvalidState | 1 |
| WF-04 | no | `design/features/13-wire-format.feature` | 返回 InternalValidationKind::InvalidState 且不发布 State | Feature: wire 格式与 golden 校验 + Scenario Outline: header 冻结字段非法值被严格拒绝 | 3 |
| WF-05 | no | `design/features/13-wire-format.feature` | 结果为 <result> | Feature: wire 格式与 golden 校验 + Scenario Outline: header record_kind 仅接受 Checkpoint 和 Delta | 5 |
| WF-06 | no | `design/features/13-wire-format.feature` | 结果为 <result>，且 RecordSeq 只由该 block_seq 注入，8 字节 header 和 body 均不再保存第二个 record sequence | Feature: wire 格式与 golden 校验 + Scenario Outline: append-log block_seq 严格转换为 RecordSeq | 5 |
| WF-07 | no | `design/features/13-wire-format.feature` | 字段顺序符合第15.3-15.4节，header 不含 application metadata 或 body length，bindings 后剩余 bytes 原样交给 application codec | Feature: wire 格式与 golden 校验 + Scenario: checkpoint 按冻结顺序编码完整 State | 1 |
| WF-08 | no | `design/features/13-wire-format.feature` | 编码成功且 device_count=1000000 | Feature: wire 格式与 golden 校验 + Scenario: registry count 一百万可编码 | 1 |
| WF-09 | no | `design/features/13-wire-format.feature` | 编码被拒绝且不分配对应集合 | Feature: wire 格式与 golden 校验 + Scenario: registry count 一百万零一被拒绝 | 1 |
| WF-10 | no | `design/features/13-wire-format.feature` | 两段 registry bytes 完全相同 | Feature: wire 格式与 golden 校验 + Scenario: 后续 checkpoint registry 字节等价 | 1 |
| WF-11 | yes | `design/features/13-wire-format.feature` | 每项仅编码 DeviceKey、BlockDeviceId、DeviceLabel 和 BlockDeviceState，wire 中不包含 Path、URI、locator 或 credential 字节 | Feature: wire 格式与 golden 校验 + Scenario: registry wire 只持久化 key、ID、label 和 device State | 1 |
| WF-12 | no | `design/features/13-wire-format.feature` | 所有 wire 字段均不包含 TransactionId 且 recovery 不从 wire 重建 TransactionId | Feature: wire 格式与 golden 校验 + Scenario: TransactionId 不进入 checkpoint 或 Delta wire | 1 |
| WF-13 | no | `design/features/13-wire-format.feature` | transaction_tag=1、change_tag=<tag>、flags=0、payload_len 精确且 payload 为 <fields> 并 roundtrip | Feature: wire 格式与 golden 校验 + Scenario Outline: segment SystemChange Delta item roundtrip | 4 |
| WF-14 | no | `design/features/13-wire-format.feature` | transaction_tag=1、change_tag=<tag>、flags=0、payload_len 精确且 payload 为 <fields> 并 roundtrip | Feature: wire 格式与 golden 校验 + Scenario Outline: large SystemChange Delta item roundtrip | 3 |
| WF-15 | no | `design/features/13-wire-format.feature` | transaction_tag=1、change_tag=<tag>、flags=0、payload_len 精确且 payload 为 BlockRef 并 roundtrip | Feature: wire 格式与 golden 校验 + Scenario Outline: root SystemChange Delta item roundtrip | 2 |
| WF-16 | no | `design/features/13-wire-format.feature` | transaction_tag=1、change_tag=<tag>、flags=0、payload_len 精确且 payload 为 <fields> 并 roundtrip | Feature: wire 格式与 golden 校验 + Scenario Outline: binding SystemChange Delta item roundtrip | 2 |
| WF-17 | no | `design/features/13-wire-format.feature` | transaction_tag=2、change_tag=0、flags=0、payload_len 精确且 roundtrip 恢复相同 Bytes | Feature: wire 格式与 golden 校验 + Scenario: Application Delta item 使用冻结 envelope | 1 |
| WF-18 | no | `design/features/13-wire-format.feature` | 返回 InternalValidationKind::InvalidState 且不发布 State | Feature: wire 格式与 golden 校验 + Scenario Outline: Delta item 非零 flags 被严格拒绝 | 2 |
| WF-19 | no | `design/features/13-wire-format.feature` | changes 顺序仍为 A、X、B | Feature: wire 格式与 golden 校验 + Scenario: Delta 保留 System 和 Application 原始顺序 | 1 |
| WF-20 | no | `design/features/13-wire-format.feature` | 连续处理恰好 15 个 level 且不存在 level_count | Feature: wire 格式与 golden 校验 + Scenario: 固定 State 恰好编码十五个 levels | 1 |
| WF-21 | no | `design/features/13-wire-format.feature` | 返回 InternalValidationKind::InvalidState | Feature: wire 格式与 golden 校验 + Scenario: LevelVersions 窗口尾部必须为 Some Active | 1 |
| WF-22 | no | `design/features/13-wire-format.feature` | 返回 InternalValidationKind::InvalidState | Feature: wire 格式与 golden 校验 + Scenario: allocation runs 和 released_count 必须一致 | 1 |
| WF-23 | no | `design/features/13-wire-format.feature` | 返回 InternalValidationKind::InvalidState | Feature: wire 格式与 golden 校验 + Scenario: release 时序不得早于 allocation 或晚于当前 record | 1 |
| WF-24 | no | `design/features/13-wire-format.feature` | 所有值按第15.3节 roundtrip 且不编码物理物化标志 | Feature: wire 格式与 golden 校验 + Scenario: Option、runs 和 lifecycle 使用显式编码 | 1 |
| WF-25 | yes | `design/features/13-wire-format.feature` | 返回 <kind> 且不发布 State | Feature: wire 格式与 golden 校验 + Scenario Outline: wire 非法输入被严格拒绝 | 12 |
| WF-26 | no | `design/features/13-wire-format.feature` | 返回 InternalValidationKind::InvalidState 且不发布 State | Feature: wire 格式与 golden 校验 + Scenario: 重复 registry 项被拒绝 | 1 |
| WF-27 | no | `design/features/13-wire-format.feature` | 返回 InternalValidationKind::InvalidState 且不发布 State | Feature: wire 格式与 golden 校验 + Scenario: 非升序 map 项被拒绝 | 1 |
| WF-28 | no | `design/features/13-wire-format.feature` | 返回 InternalValidationKind::InvalidState 且不发布 State | Feature: wire 格式与 golden 校验 + Scenario: System segment payload 的 reserved bytes 必须为零 | 1 |
| WF-29 | no | `design/features/13-wire-format.feature` | 返回 InternalValidationKind::InvalidState 且不发布 State | Feature: wire 格式与 golden 校验 + Scenario Outline: ReleaseSegmentBlocks 非法 ranges 被拒绝 | 4 |
| WF-30 | no | `design/features/13-wire-format.feature` | 返回 <result> | Feature: wire 格式与 golden 校验 + Scenario Outline: checkpoint 字符串和 application 长度边界 | 11 |
| WF-31 | no | `design/features/13-wire-format.feature` | remainder 原样交给 codec，codec 未消费或存在非法尾随时返回 InternalValidationKind::InvalidState 且不发布 State | Feature: wire 格式与 golden 校验 + Scenario: checkpoint application remainder 必须完整交给 codec | 1 |
| WF-32 | no | `design/features/13-wire-format.feature` | 返回 InternalValidationKind::InvalidState 且不发布 State | Feature: wire 格式与 golden 校验 + Scenario Outline: checkpoint 有序集合拒绝重复和乱序 | 6 |
| WF-33 | no | `design/features/13-wire-format.feature` | 结果为 <result> | Feature: wire 格式与 golden 校验 + Scenario Outline: ReleaseSegmentBlocks range 编码边界 | 6 |
| WF-34 | no | `design/features/13-wire-format.feature` | 返回 <result> | Feature: wire 格式与 golden 校验 + Scenario Outline: application Delta 长度边界 | 3 |
| WF-35 | no | `design/features/13-wire-format.feature` | 返回 <result> | Feature: wire 格式与 golden 校验 + Scenario Outline: 所有 collection count 在分配前校验上限 | 17 |
| WF-36 | no | `design/features/13-wire-format.feature` | 返回 RecordTooLarge 且不调用 append log | Feature: wire 格式与 golden 校验 + Scenario: 整条 body 超过 u32 envelope 上限时编码失败 | 1 |
| WF-37 | no | `design/features/13-wire-format.feature` | 返回 <result> | Feature: wire 格式与 golden 校验 + Scenario Outline: State ID 与 lifecycle 数值边界 | 8 |
| WF-38 | no | `design/features/13-wire-format.feature` | 返回 <result> | Feature: wire 格式与 golden 校验 + Scenario Outline: allocation runs 必须使用严格递增 sequence 和非零 len | 4 |
| WF-39 | no | `design/features/13-wire-format.feature` | 返回 <result> | Feature: wire 格式与 golden 校验 + Scenario Outline: LevelVersions 窗口结构必须合法 | 4 |
| WF-40 | no | `design/features/13-wire-format.feature` | 返回 OpenError::RecoveryFailed、kind=Decode 且 source.kind=InternalValidationKind::UnknownTag | Feature: wire 格式与 golden 校验 + Scenario: unknown record_kind 经 open 映射 Decode | 1 |
| WF-41 | yes | `design/features/13-wire-format.feature` | generator 不复用生产 codec 且双方输出逐字节一致 | Feature: wire 格式与 golden 校验 + Scenario: 独立 fixture generator 不调用生产 codec | 1 |
| WF-42 | n/a | `design/features/13-wire-format.feature:2` | 当前 feature 未提供可对应的 Scenario/Scenario Outline | Feature: wire 格式与 golden 校验 + 未映射 | 0 |
| E2E-01 | yes | `design/features/14-three-device-e2e.feature` | registry 固定 index=DeviceKey 1、hot=DeviceKey 2、cold=DeviceKey 3 | Feature: index、hot、cold 三设备端到端事务 + Scenario: create 固定三设备 registry | 1 |
| E2E-02 | no | `design/features/14-three-device-e2e.feature` | 三个 participant 的物理数据均写入各自 backend 且 metadata append 尚未重复执行 | Feature: index、hot、cold 三设备端到端事务 + Scenario: 三设备物理写按显式 placement 完成 | 1 |
| E2E-03 | no | `design/features/14-three-device-e2e.feature` | 仅追加一个包含全部有序 changes 的 Delta | Feature: index、hot、cold 三设备端到端事务 + Scenario: 三设备提交只追加一个 Delta | 1 |
| E2E-04 | no | `design/features/14-three-device-e2e.feature` | root、application 和三个设备 lifecycle 在同一 record_seq 可见 | Feature: index、hot、cold 三设备端到端事务 + Scenario: 三设备 root 和 application 在同一 State 发布 | 1 |
| E2E-05 | yes | `design/features/14-three-device-e2e.feature` | 恢复相同 registry、record_seq、roots、application 和 retirement responsibility | Feature: index、hot、cold 三设备端到端事务 + Scenario: crash 后 reopen 恢复三设备 durable State | 1 |
| E2E-06 | no | `design/features/14-three-device-e2e.feature` | 返回 SuperblockError::Device、operation=write、DeviceKey=<key>、保留 source、不发布 State、停止后续 participant、全部已取得 session abandon、transaction closed 且 gate released | Feature: index、hot、cold 三设备端到端事务 + Scenario Outline: 三设备 write 故障关闭全部 participant | 3 |
| E2E-07 | no | `design/features/14-three-device-e2e.feature` | 返回 SuperblockError::Device、operation=flush、DeviceKey=<key>、保留 source、metadata append 未开始、停止后续 key、全部 session abandon、transaction closed 且 gate released | Feature: index、hot、cold 三设备端到端事务 + Scenario Outline: 三设备 flush 故障关闭全部 participant | 3 |
| E2E-08 | no | `design/features/14-three-device-e2e.feature` | 返回 CommitStatusUnknown、State 不发布、全部 session 各 preserve 一次、零 abandon、transaction closed、gate released、status=RecoveryRequired 且必须 open | Feature: index、hot、cold 三设备端到端事务 + Scenario: 三设备 append Unknown 保留全部 participant | 1 |
| E2E-09 | no | `design/features/14-three-device-e2e.feature` | 返回 CommitFailed、State 和 RecordSeq 不变、全部 session 各 abandon 一次、零 preserve、transaction closed 且 gate 可重入 | Feature: index、hot、cold 三设备端到端事务 + Scenario: 三设备 append NotAppended 放弃全部 participant | 1 |
| E2E-10 | no | `design/features/14-three-device-e2e.feature` | 返回 Ok(CommitResult)、published State 保持、三个 session 各 finish 一次、诊断记录 DeviceKey=<key>、status=RecoveryRequired、gate released 且 reopen 可恢复 | Feature: index、hot、cold 三设备端到端事务 + Scenario Outline: 三设备 finish 故障保留 durable State | 3 |
| E2E-11 | no | `design/features/14-three-device-e2e.feature` | 返回 Ok(CommitResult)、published State 保持、三个 participant 各 enqueue_retirements 一次、诊断记录 DeviceKey=<key>、status=RecoveryRequired 且 gate released | Feature: index、hot、cold 三设备端到端事务 + Scenario Outline: 三设备 enqueue 故障返回 RecoveryRequired | 3 |
| E2E-12 | no | `design/features/14-three-device-e2e.feature` | open 重新登记 retirement 并返回 Ready | Feature: index、hot、cold 三设备端到端事务 + Scenario Outline: 三设备 enqueue 故障由 reopen 补偿 | 3 |
| E2E-13 | no | `design/features/14-three-device-e2e.feature` | 已取得的 index 和 hot backend 资源释放且不返回 device map、snapshot lease 或 Superblock | Feature: index、hot、cold 三设备端到端事务 + Scenario: cold 离线失败不保留部分运行时资源 | 1 |
| E2E-14 | yes | `design/features/14-three-device-e2e.feature` | 旧 snapshot 仍读取原数据且新 snapshot 拒绝已 removed BlockRef | Feature: index、hot、cold 三设备端到端事务 + Scenario: release 前旧 snapshot 在新提交后继续读取 | 1 |
| E2E-15 | no | `design/features/14-three-device-e2e.feature` | enqueue 立即返回且 worker 继续等待 lease | Feature: index、hot、cold 三设备端到端事务 + Scenario: worker 慢回收不阻塞同步登记 | 1 |
| E2E-16 | no | `design/features/14-three-device-e2e.feature` | SuperblockStatus 不变且后续 worker 可重试 | Feature: index、hot、cold 三设备端到端事务 + Scenario: worker 删除失败不设置恢复围栏 | 1 |
| E2E-17 | yes | `design/features/14-three-device-e2e.feature` | 返回 OpenError::RecoveryFailed、kind=ResolveDevice、保留 cold DeviceKey 和 source 且不返回部分实例 | Feature: index、hot、cold 三设备端到端事务 + Scenario: cold 离线使整体 reopen 返回完整 OpenError | 1 |
| RM-01 | no | `design/features/15-release-metadata.feature` | 同时声明 <path> 和 crates.io version <version> | Feature: 三 crate 发布元数据与边界 + Scenario Outline: 两条 workspace 依赖边同时声明 path 和 version | 2 |
| RM-02 | no | `design/features/15-release-metadata.feature` | version 为 <version> | Feature: 三 crate 发布元数据与边界 + Scenario Outline: 每个 crate 使用冻结 package version | 3 |
| RM-03 | yes | `design/features/15-release-metadata.feature` | license=MIT、package archive 含 LICENSE、repository 指向当前 origin 且不执行任何规范化，manifest repository 必须与当前 origin 字符串逐字节精确相等、docs.rs metadata 设置 all-features=true、rust-version=1.85、readme=<readme> 且 changelog=<changelog> | Feature: 三 crate 发布元数据与边界 + Scenario Outline: 每个 crate 声明精确发布元数据 | 3 |
| RM-04 | no | `design/features/15-release-metadata.feature` | 不调用 git config、不修改 remote URL 且仅报告 manifest repository 是否与当前 origin 字符串逐字节精确相等 | Feature: 三 crate 发布元数据与边界 + Scenario: repository 校验不修改 git 配置 | 1 |
| RM-05 | yes | `design/features/15-release-metadata.feature` | package 成功且 archive 中 Cargo.toml、README、CHANGELOG 和 LICENSE 可解析 | Feature: 三 crate 发布元数据与边界 + Scenario Outline: 每个 crate package 成功且包内容可解析 | 3 |
| RM-06 | no | `design/features/15-release-metadata.feature` | dry-run 成功且依赖解析不引用尚未按顺序发布的 workspace 包 | Feature: 三 crate 发布元数据与边界 + Scenario Outline: 每个 crate 在先决依赖可由 registry 解析后 dry-run 成功 | 3 |
| RM-07 | no | `design/features/15-release-metadata.feature` | 先验证 pi_block_device，再在 registry 可解析其 0.2.0 后验证 pi_file_block_device 和 pi_superblock | Feature: 三 crate 发布元数据与边界 + Scenario: dry-run 按依赖先决顺序执行 | 1 |
| RM-08 | yes | `design/features/15-release-metadata.feature` | 顺序为 pi_block_device、pi_file_block_device、pi_superblock | Feature: 三 crate 发布元数据与边界 + Scenario: 发布顺序遵守依赖方向 | 1 |
| RM-09 | no | `design/features/15-release-metadata.feature` | 明确声明 pi_block_device 0.1 持久数据无迁移且本版为新布局 | Feature: 三 crate 发布元数据与边界 + Scenario: 发布说明声明无旧布局迁移 | 1 |
| RM-10 | no | `design/features/15-release-metadata.feature` | 包含 <claims> | Feature: 三 crate 发布元数据与边界 + Scenario Outline: 各 crate release notes 声明自身交付边界 | 3 |
| AB-01 | yes | `design/features/16-architecture-boundaries.feature` | pi_superblock 依赖 pi_block_device 和 pi_append_log且绝不依赖 pi_file_block_device | Feature: 三 crate 架构边界 + Scenario: pi_superblock 只依赖协议层和 append log | 1 |
| AB-17 | yes | `design/features/16-architecture-boundaries.feature` | 使用 pi_append_log 0.3.0 的多段写和异步流写能力，并通过 pi-async-fs 完成异步文件 I/O，不退回逐段同步写 | Feature: 三 crate 架构边界 + Scenario: pi_superblock 使用 append log 0.3 多段和流写 | 1 |
| AB-02 | no | `design/features/16-architecture-boundaries.feature` | pi_file_block_device 依赖 pi_block_device 且不承担 superblock 日志职责 | Feature: 三 crate 架构边界 + Scenario: pi_file_block_device 依赖 pi_block_device | 1 |
| AB-03 | no | `design/features/16-architecture-boundaries.feature` | 这些具体实现不存在 | Feature: 三 crate 架构边界 + Scenario: pi_block_device 不包含具体文件实现 | 1 |
| AB-04 | yes | `design/features/16-architecture-boundaries.feature` | Superblock 没有 log 泛型且不存在 public log trait | Feature: 三 crate 架构边界 + Scenario: Superblock 不暴露 public log trait 或日志泛型 | 1 |
| AB-05 | no | `design/features/16-architecture-boundaries.feature` | 这些 API 不存在 | Feature: 三 crate 架构边界 + Scenario: 公开 API 不提供动态设备 change | 1 |
| AB-06 | no | `design/features/16-architecture-boundaries.feature` | 公开签名不引用 Tokio 类型且保持 runtime-neutral | Feature: 三 crate 架构边界 + Scenario: 生产 API 不引用 Tokio 类型 | 1 |
| AB-07 | no | `design/features/16-architecture-boundaries.feature` | 两者均不在生产依赖中 | Feature: 三 crate 架构边界 + Scenario: 生产 API 不依赖 async-trait 或 serde | 1 |
| AB-08 | no | `design/features/16-architecture-boundaries.feature` | 只有 pi_superblock 声明 futures-core | Feature: 三 crate 架构边界 + Scenario: futures-core 只由 pi_superblock 使用 | 1 |
| AB-09 | no | `design/features/16-architecture-boundaries.feature` | pi_superblock 可声明生产依赖且 pi_file_block_device 仅可声明开发依赖 | Feature: 三 crate 架构边界 + Scenario: futures-lite 只在批准范围使用 | 1 |
| AB-10 | no | `design/features/16-architecture-boundaries.feature` | smallvec 不在任何生产依赖中 | Feature: 三 crate 架构边界 + Scenario: smallvec 不进入生产依赖 | 1 |
| AB-11 | no | `design/features/16-architecture-boundaries.feature` | 检查成功且依赖版本未被锁文件替换 | Feature: 三 crate 架构边界 + Scenario: 依赖版本和 Rust 版本固定 | 1 |
| AB-12 | no | `design/features/16-architecture-boundaries.feature` | 使用 pi-async-fs 完成异步文件操作、多段写和异步流写，不直接声明或使用 memmap2；Active、Sealed 和 large 的已完成不可变范围均使用 pi-async-fs 不透明只读 mmap 句柄，普通段按固定 slot offset 映射且 append 后仅建立新不相交范围 | Feature: 三 crate 架构边界 + Scenario: 文件后端使用 pi-async-fs 且不使用 memmap2 | 1 |
| AB-13 | no | `design/features/16-architecture-boundaries.feature` | 两个底层 crate 不暴露 application log 或 superblock State | Feature: 三 crate 架构边界 + Scenario: Application codec 不越过 superblock 边界 | 1 |
| AB-14 | no | `design/features/16-architecture-boundaries.feature` | pi_superblock 只接收统一 backend 类型 D | Feature: 三 crate 架构边界 + Scenario: enum adapter 统一异构 backend | 1 |
| AB-15 | no | `design/features/16-architecture-boundaries.feature` | 旧 facade 不作为公开发布边界存在 | Feature: 三 crate 架构边界 + Scenario: 无旧 aggregate facade | 1 |
| AB-16 | yes | `design/features/16-architecture-boundaries.feature` | 仅内部 SuperblockAppendLog 恢复的完整逻辑 State 具有权威性，manifest、介质 metadata 和 retirement queue 只能被校验、修复或从该 State 重新推导且不得以冲突内容覆盖 State | Feature: 三 crate 架构边界 + Scenario: 单一内部 pi_append_log 是完整逻辑 State 唯一持久权威 | 1 |
