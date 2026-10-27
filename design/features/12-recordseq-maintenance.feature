# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: RS-01, RS-02, RS-28, RS-03, RS-04, RS-05, RS-06, RS-07, RS-08, RS-09, RS-10, RS-11, RS-12, RS-13, RS-14, RS-15, RS-16, RS-17, RS-18, RS-19, RS-20, RS-21, RS-22, RS-23, RS-24, RS-25, RS-26, RS-27
Feature: RecordSeq 与日志维护
  # 最新批准：显式维护整程持 writer gate，包含 archive；phase 是日志安全次序而非事务阶段。
  # 首次归档与重试共用 pending_archive Mutex<VecDeque>，每轮只处理固定快照。
  # 默认生产外部维护入口签名未批准；现有 cfg(test-support) 入口不是生产 API 验收。
  作为日志维护者
  我希望序号耗尽、轮转、checkpoint 和 archive 顺序固定
  以便恢复边界可证明

  @critical
  Scenario: MAX-2 发布 MAX-1 后立即只读耗尽
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 当前 record_seq 为 u32::MAX-2
    When 业务提交发布 u32::MAX-1
    Then commit 返回成功且 SuperblockStatus 立即为 ReadOnlyExhausted

  @critical
  Scenario: ReadOnlyExhausted 拒绝公开 Superblock::begin_write 且零副作用
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given Superblock 的 record_seq 为 u32::MAX-1 且 status=ReadOnlyExhausted
    When 调用公开 Superblock::begin_write
    Then 精确返回 SuperblockError::ReadOnlyExhausted、writer gate 不被取得或等待、不创建 Transaction 或 participant、backend begin_write_batch、write 和 prepare 均未调用、candidate 和 published State 不变、RecordSeq 不分配或消费、metadata append 未调用且 maintenance phase、archive retry queue 和 retirement queue 不变

  Scenario Outline: ReadOnlyExhausted 拒绝内部 log maintenance 触发且零副作用
    Given Superblock 的 record_seq 为 u32::MAX-1 且 status=ReadOnlyExhausted
    When 内部 log_maintenance 尝试触发 <trigger>
    Then 返回内部复用的 SuperblockError::ReadOnlyExhausted、不取得 writer gate、不改变 maintenance phase、archive retry queue 或 retirement queue、不分配 RecordSeq、不调用 metadata append 且不产生任何日志或介质元数据副作用

    Examples:
      | trigger |
      | rotate |
      | checkpoint |
      | archive retry |

  Scenario: checkpoint 消费最后合法序号立即只读耗尽
    Given maintenance 当前 record_seq=u32::MAX-2
    When checkpoint 使用 u32::MAX-1 durable 并发布
    Then published status 为 ReadOnlyExhausted 且不再执行 archive retry

  Scenario: ReadOnlyExhausted 重启后保持
    Given 日志最终 envelope sequence 为 u32::MAX-1
    When 重新 open
    Then open 返回 ReadOnlyExhausted 状态且 read 和 snapshot 仍可用

  Scenario: Active 转换为 RotateRequested
    Given Active 的 active_size 达到 rotate threshold
    When maintenance 检查阈值
    Then phase 从 Active 转为 RotateRequested

  Scenario: RotateRequested 转换为 Rotating
    Given phase 为 RotateRequested 且 writer gate 已取得并捕获最新 State
    When maintenance 开始 rotate
    Then phase 转为 Rotating

  Scenario: rotate no-op 返回 Active
    Given phase 为 Rotating 且底层 rotate 判定无需关闭 structure
    When rotate 返回 no-op
    Then phase 转回 Active 且不分配 RecordSeq

  Scenario: rotate 成功转换为 CheckpointRequired
    Given phase 为 Rotating
    When rotate 关闭旧 structure 并创建新空 Active
    Then phase 转为 CheckpointRequired

  @critical
  Scenario: maintenance checkpoint 完整前半与发布归档顺序
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given maintenance 取得 writer gate 后读取最新 published State.record_seq=R 且 R+1<u32::MAX-1
    When 执行一次需要 rotate 的 checkpoint maintenance
    Then 编码最新完整 State 的 R+1 checkpoint、rotate 创建新 Active、以 R+1 将 checkpoint 作为新 Active 首记录 durable、生成候选并原子 publish、进入 ArchivePending 后 archive 旧 structure、最后释放 writer gate；完整 checkpoint 编码仍为实现目标

  @critical
  Scenario: 非耗尽 checkpoint 事件顺序为 durable、publish、archive、gate release
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given phase=CheckpointRequired、当前 State.record_seq=R 且 R+1<u32::MAX-1
    When R+1 checkpoint 完成维护
    Then 事件严格为 durable R+1、原子 publish、进入 ArchivePending 后 archive、最后 writer gate release

  Scenario: checkpoint 发布后下一业务使用 R+2
    Given R+1 checkpoint 已 durable 并发布且 gate 已释放
    When 下一业务事务分配 RecordSeq
    Then 分配 R+2

  Scenario: 新 Active 首条 Delta 导致 RecoveryFailed
    Given rotate 后新 Active 的第一条完整记录是 Delta
    When open 恢复该 structure
    Then 返回 OpenError::RecoveryFailed、kind=RecoveryFailureKind::Decode 且不发布 Superblock

  Scenario: ArchivePending 当前旧 structure 成功后返回 Active
    Given phase 为 ArchivePending 且当前待归档旧 structure 已确定
    When 当前旧 structure archive 成功
    Then phase 转为 Active

  Scenario: ArchivePending 当前旧 structure 失败后进入 ArchiveRetry
    Given phase 为 ArchivePending
    When 当前旧 structure archive 失败
    Then 旧 structure 加入 retry queue 且 phase 转为 ArchiveRetry

  Scenario: ArchiveRetry 失败时保持 ArchiveRetry
    Given phase 为 ArchiveRetry 且 retry queue 非空
    When 重试队首 structure archive 失败
    Then queue 保留未完成项且 phase 保持 ArchiveRetry

  Scenario: ArchiveRetry 成功且队列清空后返回 Active
    Given phase 为 ArchiveRetry 且 retry queue 仅有一个 structure
    When 该 structure archive 成功
    Then retry queue 为空且 phase 转为 Active

  Scenario: ArchiveRetry 成功但队列非空时保持 ArchiveRetry
    Given phase 为 ArchiveRetry 且 retry queue 含多个 structure
    When 队首 structure archive 成功但仍有待处理项
    Then retry queue 仍非空且 phase 保持 ArchiveRetry

  Scenario: no-op maintenance 保持 Active
    Given Active 未达到 threshold 且没有维护请求
    When maintenance 检查当前状态
    Then phase 保持 Active 且不追加维护记录

  @critical
  Scenario: rotate error 仅建立恢复围栏
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given phase 为 Rotating
    When rotate 返回 error
    Then record_seq 不变、禁止继续 append、SuperblockStatus=RecoveryRequired 且调用方必须 open 裁决介质状态

  Scenario: rotate unknown 建立 RecoveryRequired
    Given phase 为 Rotating 且 rotate I/O 已开始
    When rotate 结果为 unknown
    Then record_seq 不发布、禁止继续 append、SuperblockStatus=RecoveryRequired 且调用方必须 open 裁决介质状态

  Scenario Outline: rotate 失败后介质未变可由 open 恢复
    Given rotate <outcome> 后旧完整 structure 仍是唯一可恢复结构
    When 调用 open 裁决日志
    Then open 从旧 structure 恢复原 record_seq 并返回 Ready

    Examples:
      | outcome |
      | error |
      | unknown |

  Scenario Outline: rotate 失败后介质状态不确定由 open 拒绝
    Given rotate <outcome> 后无法证明旧 structure 或新 Active 的 durable 状态
    When 调用 open 裁决日志
    Then 返回 OpenError::RecoveryFailed、kind=LogStateIndeterminate 且不返回 Superblock

    Examples:
      | outcome |
      | error |
      | unknown |

  Scenario: rotate 后 checkpoint NotAppended 建立 RecoveryRequired
    Given phase 为 CheckpointRequired 且新 Active 已创建
    When checkpoint append 返回 NotAppended
    Then record_seq 不变、新 Active 不可追加、SuperblockStatus=RecoveryRequired 且 open 可从旧完整 structure 恢复

  @critical
  Scenario: rotate 后 checkpoint Unknown 建立 RecoveryRequired
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given phase 为 CheckpointRequired 且 checkpoint I/O 已开始
    When checkpoint append 返回 Unknown
    Then record_seq 不发布、新 Active 不可追加、SuperblockStatus=RecoveryRequired 且必须 open 裁决日志

  Scenario: rotate 后 checkpoint cancellation 建立 RecoveryRequired
    Given phase 为 CheckpointRequired 且 checkpoint append future 正在等待
    When 调用方取消该 future
    Then 不产生 DurableAppendOutcome、R+1 不发布、SuperblockStatus=RecoveryRequired 且必须 open 裁决

  Scenario: 最后合法 checkpoint 发布后进入首次 archive
    Given R+1=u32::MAX-1 的 checkpoint 已 durable
    When candidate 发布为 ReadOnlyExhausted
    Then 事件严格为 publish ReadOnlyExhausted、进入 ArchivePending 后首次 archive、最后 writer gate release；首次 archive 失败时保留统一队列责任但 ReadOnlyExhausted 不执行 archive retry

  Scenario: checkpoint durable 后 publish failure 建立 RecoveryRequired
    Given R+1 checkpoint 已 durable
    When candidate 原子 publish 失败
    Then R+1 保持已消费且 SuperblockStatus 为 RecoveryRequired
