# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: CO-01, CO-02, CO-03, CO-04, CO-05, CO-06, CO-07, CO-08, CO-09, CO-10, CO-11, CO-12
# 方案 A 声明阶段：受影响运行时为 todo 占位；以下为待实现合同，不是验收通过或目录 fsync 已实现的声明。
# prepare(self) 消费批次；成功时数据完整、最终布局可恢复定位、必要文件 DataAndMetadata sync 真实成功先于 metadata durable；2026-10-08 排除目录项突然掉电未落盘，不表示父目录 fsync。
# Drop 仅释放资源，不删除可能已提交的数据；未提交残留由权威恢复状态驱动 reconcile，Unknown 由 pi_superblock 恢复裁决。
# 旧规则 ID 映射及旧测试待迁移，旧三终态规则已失效。
# 2026-10-11 phase/closed/WriteGuard 已移除；下述顺序是未实现算法目标，不是阶段字段。
# commit(&mut self) 不改消费式；取消围栏/终态状态未提供，不恢复旧机制或默认成功。
Feature: commit、create 三态与公开状态
  作为可靠性调用方
  我希望提交顺序、participant 和 append outcome 被精确表达
  以便 durable 成功不会被错误报告为失败

  @critical
  Scenario: append NotAppended 完整回滚未发布事务
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given metadata append 返回 DurableAppendOutcome::NotAppended
    When commit 完成明确未提交清理
    Then 返回 CommitFailed、candidate 和 published State 不变、RecordSeq 未消费、批次仅释放本地资源，残留由权威恢复状态驱动 reconcile 且下一事务可取得 gate

  @critical
  Scenario: append Durable 普通序号返回 Ready
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given metadata append Durable 的 record_seq=R 且 R<u32::MAX-1
    When publish 和 enqueue_retirements 全部成功
    Then 返回 Ok(CommitResult { record_seq: R, status: Ready })

  Scenario: append Durable 最后合法序号返回 ReadOnlyExhausted
    Given metadata append Durable 的 record_seq=u32::MAX-1
    When publish 和 enqueue_retirements 全部成功
    Then 返回 Ok(CommitResult { record_seq: u32::MAX-1, status: ReadOnlyExhausted })

  Scenario: append Unknown 保留 participant 并围栏
    Given metadata append 返回 DurableAppendOutcome::Unknown
    When commit 裁决结果
    Then 返回 CommitStatusUnknown、仅释放本地资源而不删除可能已提交的数据，Unknown 由 pi_superblock 恢复裁决、终态/取消协议未提供、status=RecoveryRequired 且必须 open

  Scenario: append future cancellation 没有 DurableAppendOutcome
    Given append future 已开始并持有全部 participant batch
    When 调用方取消 append future
    Then 不产生 DurableAppendOutcome、仅释放本地资源而不删除可能已提交的数据，Unknown 由 pi_superblock 恢复裁决、终态/取消协议未提供、status=RecoveryRequired 且必须 open

  Scenario Outline: durable 后单个 participant 故障返回成功事实
    Given metadata append 已 durable 且 <participant> 返回错误
    When commit 完成结果裁决
    Then 返回 Ok(CommitResult) 且 status=RecoveryRequired

    Examples:
      | participant |
      | publish |
      | enqueue_retirements |

  Scenario: publish failure 不撤销 durable 业务结果
    Given Delta 已 durable 且 candidate publish 失败
    When commit 返回
    Then RecordSeq 保持已消费且 status=RecoveryRequired

  Scenario: enqueue failure 交给 reopen 补偿
    Given Delta 已 durable 且 enqueue_retirements 失败
    When commit 返回后重新 open
    Then commit status=RecoveryRequired 且 open 重新登记 retirement

  @critical
  Scenario: commit 操作顺序可观察
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 一个包含多个设备 batch 的事务
    When commit 执行
    Then 操作顺序为 prevalidate、按 key 通过 Option::take 消费 batch 并 await prepare、单次 append、publish、按 key enqueue_retirements

  Scenario: SuperblockStatus 名称固定
    Given Superblock 处于任一公开运行状态
    When 调用 status
    Then 结果只能是 Ready、ReadOnlyExhausted 或 RecoveryRequired
