# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: TX-01, TX-02, TX-03, TX-04, TX-05, TX-06, TX-07, TX-08, TX-09, TX-10, TX-11, TX-12, TX-13, TX-14, TX-15, TX-16, TX-17, TX-18, TX-19, TX-20, TX-21, TX-22, TX-23, TX-24, TX-25, TX-26, TX-27, TX-28, TX-29, TX-30, TX-31, TX-32, TX-33, TX-34, TX-35, TX-36, TX-37, TX-38
# 当前事务主链已实现，生产源码无 todo 占位；具体验收范围见 WORK，不声明目录 fsync。
# prepare(self) 消费批次；成功时数据完整、最终布局可恢复定位、必要文件 DataAndMetadata sync 真实成功先于 metadata durable；2026-10-08 排除目录项突然掉电未落盘，不表示父目录 fsync。
# Drop 仅释放资源，不删除可能已提交的数据；未提交残留由权威恢复状态驱动 reconcile，Unknown 由 pi_superblock 恢复裁决。
# 旧规则 ID 映射为历史索引；旧测试已迁移，旧三终态规则已失效。
# 2026-10-11 撤销 TransactionId/allocator/跨事务身份归属及 phase/closed/WriteGuard 合同。
# Transaction<'a,S,C,D,R> 只 borrow 字段，单事务借用 async mutex guard；commit 保留 &mut self。
# 当前以 writer_guard 是否存在判断关闭，命令准入已实现；metadata append 使用局部取消围栏，Pending/Unknown 专项动态验收仍待补。
Feature: 事务顺序与受控变化
  # 版本窗口使用 rpds VectorSync；2026-10-12已实现前部空版本裁剪/min_version推进，无2window限制。
  # apply_system_change纯状态candidate成功替换已实现，公开recovery目标已验证；事务准入/commit 已实现。
  作为事务调用方
  我希望 SystemChange 和 ApplicationChange 按单一顺序提交
  以便运行时和恢复产生相同 State

  @critical
  Scenario: System A、Application X、System B 保持交错
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 一个事务依次产生 System A、Application X、System B
    When commit 和 recovery replay 变化
    Then 两条路径都按 A、X、B 顺序得到相同 candidate State

  Scenario: 应用不能直接 push 任意 SystemChange
    Given 外部 application 持有 Transaction API
    When application 尝试构造任意 SystemChange
    Then 公开 API 不提供该构造入口

  Scenario: AddRoot 成功登记 live BlockRef
    Given 一个 live BlockRef 尚未成为 root
    When Transaction 调用 add_root
    Then candidate roots 增加该 BlockRef

  Scenario: 重复 AddRoot 返回 S3裁决 SYSTEM-CONFLICT=SuperblockError::InvalidBlockRef 决策错误
    Given candidate roots 已包含 BlockRef B
    When Transaction 再次调用 add_root(B)
    Then 返回 SuperblockError::InvalidBlockRef 且 candidate 不发布且 backend 不调用

  Scenario: RemoveRoot 后释放同一 block 成功
    Given root B 引用仍 live 的 block
    When changes 依次执行 RemoveRoot(B)、ReleaseSegmentBlocks(B)
    Then 两个 change 均成功且最终 roots 不包含 B

  Scenario: 释放后 AddRoot 返回 S3裁决 SYSTEM-CONFLICT=SuperblockError::InvalidBlockRef 决策错误
    Given changes 已对 B 执行 ReleaseSegmentBlocks
    When 随后执行 AddRoot(B)
    Then 返回 SuperblockError::InvalidBlockRef 且 candidate 不发布且 backend 不调用

  Scenario: AddRoot 非 live block 返回 S3裁决 SYSTEM-CONFLICT=SuperblockError::InvalidBlockRef 决策错误
    Given BlockRef B 不属于 candidate 的 live block 集合
    When Transaction 调用 add_root(B)
    Then 返回 SuperblockError::InvalidBlockRef 且 candidate 不发布且 backend 不调用

  Scenario: 同名 binding 删除后重建成功
    Given name main 当前绑定 B1
    When changes 依次执行 RemoveBinding(main)、AddBinding(main,B2)
    Then 最终 main 绑定 B2

  Scenario: 重复 binding 返回 S3裁决 SYSTEM-CONFLICT=SuperblockError::InvalidBlockRef 决策错误
    Given name main 当前绑定 B1
    When Transaction 调用 bind(main,B2)
    Then 返回 SuperblockError::InvalidBlockRef 且 candidate 不发布且 backend 不调用

  Scenario: 重复 release 返回 S3裁决 SYSTEM-CONFLICT=SuperblockError::InvalidBlockRef 决策错误
    Given block B 的 released_at 已设置
    When Transaction 再次调用 release(B)
    Then 返回 SuperblockError::InvalidBlockRef 且 candidate 不发布且 backend 不调用

  Scenario: release 未分配 block 返回 S3裁决 SYSTEM-CONFLICT=SuperblockError::InvalidBlockRef 决策错误
    Given BlockRef B 不属于 candidate 的已分配 block 集合
    When Transaction 调用 release(B)
    Then 返回 SuperblockError::InvalidBlockRef 且 candidate 不发布且 backend 不调用

  Scenario: CreateActiveSegment 合法变更成功
    Given 一个 level 的 Active 已达到容量且下一个 SegmentVersion 可用
    When Transaction 生成 CreateActiveSegment
    Then candidate 追加新的虚拟 Active 且旧 Active 变为 Sealed

  Scenario: AllocateSegmentBlocks 合法变更成功
    Given 一个 Active 具有可用连续槽位
    When Transaction 生成 AllocateSegmentBlocks
    Then candidate 追加非空 AllocationRun

  Scenario: ReleaseSegmentBlocks 合法变更成功
    Given 一个普通 segment 含有尚未释放的目标范围
    When Transaction 生成 ReleaseSegmentBlocks
    Then candidate 为目标槽位写入当前 RecordSeq

  Scenario: RemoveSegment 合法变更成功
    Given 一个不受保护且全部释放的 Sealed segment
    When maintenance 生成 RemoveSegment
    Then candidate 移除该 segment 并产生 RetiredObject::Segment

  Scenario: AllocateLargeBlock 合法变更成功
    Given next large block id 仍在有效范围
    When Transaction 生成 AllocateLargeBlock
    Then candidate 插入 LargeBlockData 并推进 next_id

  Scenario: ReleaseLargeBlock 合法变更成功
    Given 一个尚未释放的 LargeBlockData
    When Transaction 生成 ReleaseLargeBlock
    Then candidate 写入当前 RecordSeq 的 released_at

  Scenario: RemoveLargeBlock 合法变更成功
    Given 一个不受保护且已释放的 large block 文件长度正确
    When maintenance 生成 RemoveLargeBlock
    Then candidate 移除 entry 并产生 RetiredObject::LargeBlock

  Scenario: 受控 SystemChange 冲突返回 S3裁决 SYSTEM-CONFLICT=SuperblockError::InvalidBlockRef 决策错误
    Given 一个 SystemChange 的目标状态不满足批准协议前置条件
    When replay 该 SystemChange
    Then 返回 SuperblockError::InvalidBlockRef 且 candidate 不发布且 backend 不调用

  @critical
  Scenario: single writer gate 串行化持久修改
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 两个调用方同时请求 begin_write
    When writer gate 调度请求
    Then 一个事务先取得 gate 且另一个事务等待其释放

  Scenario: commit 失败后 batch 和 gate 可观察释放
    Given 事务的 metadata append 返回 NotAppended
    When commit 返回 CommitFailed
    Then 所有批次本地资源已释放，物理残留留待权威恢复状态驱动 reconcile且 writer gate 可被下一事务取得

  Scenario: 多设备 prepare 按 DeviceKey 升序
    Given 事务在 DeviceKey 3、1、2 上有待 prepare 的 batch
    When commit 执行物理 prepare
    Then prepare 顺序为 1、2、3

  @critical
  Scenario: 多设备事务只追加一个 Delta
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 事务在多个设备产生 SystemChange 和 ApplicationChange
    When commit 完成所有设备 prepare
    Then metadata log 只追加一个按顺序编码的 Delta

  Scenario: 恢复重建跨设备 State
    Given 一个跨设备 Delta 已 durable 且包含 roots、bindings 和 application change
    When open replay 该 Delta
    Then 恢复后的 State 与提交时发布的 State 相同

  @critical
  Scenario: write_batch 与逻辑 changes 保持严格交错边界
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given transaction 依次产生 System A、跨多个设备的 write_batch、Application B 和 System C
    When 执行 write、prepare、commit 并编码统一 TransactionChange
    Then logical change 顺序仅为 System A、Application B、System C；batch request、write、chunk、prepare、participant 和物理调度不进入 TransactionChange、ApplicationApplyContext 或 wire

  Scenario Outline: BindingName 公共构造精确校验 UTF-8 字节长度
    Given 输入 BindingName 的原始字节为 <input> 且长度为 <length>
    When 调用 BindingName 公共构造入口
    Then 返回 <result>

    Examples:
      | input | length | result |
      | 空字节 | 0 | SuperblockError::InvalidBindingName |
      | a | 1 | success |
      | 有效 UTF-8 | 1048576 | success |
      | 有效 UTF-8 | 1048577 | SuperblockError::InvalidBindingName |
      | 非法 UTF-8 字节 | 1 | SuperblockError::InvalidBindingName |

  Scenario Outline: 非法 BindingName 在 transaction 调用前零副作用失败
    Given <operation> 的名称输入为长度 <length> 的 <input>
    When 调用方构造 BindingName 后仅在成功时调用 transaction
    Then 构造返回 SuperblockError::InvalidBindingName、transaction 方法未调用、不增加 SystemChange、不调用 backend I/O 且无 append

    Examples:
      | operation | input | length |
      | bind | 空字节 | 0 |
      | bind | 非法 UTF-8 字节 | 1 |
      | bind | 有效 UTF-8 | 1048577 |
      | unbind | 空字节 | 0 |
      | unbind | 非法 UTF-8 字节 | 1 |
      | unbind | 有效 UTF-8 | 1048577 |

  Scenario Outline: bind 和 unbind 接受 BindingName 合法边界
    Given <operation> 收到长度为 <length> 的有效 UTF-8 BindingName
    When transaction 调用该操作
    Then 生成一个保持原始名称字节的受控 binding change

    Examples:
      | operation | length |
      | bind | 1 |
      | bind | 1048576 |
      | unbind | 1 |
      | unbind | 1048576 |

  @critical
  Scenario: backend 不接收事务身份
    Given 一个 transaction 首次访问 DeviceKey 3、1、2
    When 分别调用 backend begin_write_batch(self: &Arc<Self>)
    Then 构造调用没有事务身份参数，pi_superblock 也不提供事务身份分配

  Scenario: 事务资源借用不复制 backend 列表
    Given Superblock 持有 SmallVec<[Arc<D>;4]> 且发布槽为直接 ArcSwap<State>
    When 检查 Transaction<'a,S,C,D,R> 声明
    Then devices 是 &'a [Arc<D>]，codec/log/status/publication/协调字段均借用，无 owner Superblock 或 TransactionId

  Scenario: candidate 初始共享逻辑基准
    Given begin_write 的骨架合同
    When 捕获 published baseline 并建立 candidate
    Then 两者初始为同一个 Arc<State>，仅克隆 Arc，首次修改使用 Arc::make_mut，不在 begin 克隆 State

  Scenario: application 返回 candidate 的借用
    Given 一个 Transaction<'a,S,C,D,R>
    When 调用纯查询 application()
    Then 返回 &S，不新增 getter 或终态检查

  Scenario: ReadOnlyExhausted 的写入拒绝目标不依赖 closed 状态
    Given Superblock 状态为 ReadOnlyExhausted 且未被 RecoveryRequired 围栏
    When 请求 begin_write 或写入命令
    Then 返回 ReadOnlyExhausted 且零 change、I/O、append 副作用；begin 和事务命令准入仍 TODO，不提供已撤销的 closed 矩阵

  @critical
  Scenario: Transaction Drop 不删除可能提交的数据
    Given Transaction 持有尚未消费的 batches 或已开始 metadata append
    When Transaction 被丢弃
    Then 释放本地资源和 writer gate，不执行 backend 事务终态，不删除可能已提交的数据；Unknown 由 pi_superblock 恢复裁决，残留由权威恢复状态驱动 reconcile
