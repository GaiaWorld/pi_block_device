# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: E2E-01, E2E-02, E2E-03, E2E-04, E2E-05, E2E-06, E2E-07, E2E-08, E2E-09, E2E-10, E2E-11, E2E-12, E2E-13, E2E-14, E2E-15, E2E-16, E2E-17
# 方案 A 声明阶段：受影响运行时为 todo 占位；以下为待实现合同，不是验收通过或目录 fsync 已实现的声明。
# prepare(self) 消费批次；成功时数据完整、最终布局可恢复定位、必要文件 DataAndMetadata sync 真实成功先于 metadata durable；2026-10-08 排除目录项突然掉电未落盘，不表示父目录 fsync。
# Drop 仅释放资源，不删除可能已提交的数据；未提交残留由权威恢复状态驱动 reconcile，Unknown 由 pi_superblock 恢复裁决。
# 旧规则 ID 映射及旧测试待迁移，旧三终态规则已失效。
Feature: index、hot、cold 三设备端到端事务
  作为多设备应用调用方
  我希望静态三设备事务、故障和重启结果可观察
  以便跨设备引用和冷数据具有一致恢复语义

  @critical
  Scenario: create 固定三设备 registry
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given create 按 index、hot、cold 顺序声明三个在线设备
    When create 成功
    Then registry 固定 index=DeviceKey 1、hot=DeviceKey 2、cold=DeviceKey 3

  Scenario: 三设备物理写按显式 placement 完成
    Given 一个事务分别在 hot、index 和 cold 分配数据
    When 执行全部 write 和 prepare
    Then 三个 participant 的物理数据均写入各自 backend 且 metadata append 尚未重复执行

  Scenario: 三设备提交只追加一个 Delta
    Given 三个 participant 的 write 和 prepare 已成功
    When commit 追加 metadata
    Then 仅追加一个包含全部有序 changes 的 Delta

  Scenario: 三设备 root 和 application 在同一 State 发布
    Given Delta 同时包含 index root、hot leaf 引用和 cold application BlockRef
    When Delta durable 并发布
    Then root、application 和三个设备 lifecycle 在同一 record_seq 可见

  @critical
  Scenario: crash 后 reopen 恢复三设备 durable State
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 三设备物理写和单一 metadata Delta 已 durable 后进程崩溃
    When 重新 open 且三个设备在线
    Then 恢复相同 registry、record_seq、roots、application 和 retirement responsibility

  Scenario Outline: 三设备 write 故障关闭全部 participant
    Given <label> 对应 DeviceKey <key> 的 write 返回 DeviceError
    When 当前写操作返回
    Then 返回 SuperblockError::Device、operation=write、DeviceKey=<key>、保留 source、不发布 State、停止后续 participant、全部已取得批次只释放本地资源，残留由权威恢复状态驱动 reconcile、transaction closed 且 gate released

    Examples:
      | label | key |
      | index | 1 |
      | hot | 2 |
      | cold | 3 |

  Scenario Outline: 三设备 prepare 故障关闭全部 participant
    Given <label> 对应 DeviceKey <key> 的 prepare 返回 DeviceError
    When commit 在该 key 停止 prepare
    Then 返回 SuperblockError::Device、operation=prepare、DeviceKey=<key>、保留 source、metadata append 未开始、停止后续 key、全部批次只释放本地资源，残留由权威恢复状态驱动 reconcile、transaction closed 且 gate released

    Examples:
      | label | key |
      | index | 1 |
      | hot | 2 |
      | cold | 3 |

  Scenario: 三设备 append Unknown 保留全部 participant
    Given DeviceKey 1、2、3 prepare 成功且 append 返回 Unknown
    When commit 裁决结果
    Then 返回 CommitStatusUnknown、State 不发布、只释放本地资源，不删除可能已提交的数据，Unknown 由 pi_superblock 恢复裁决、transaction closed、gate released、status=RecoveryRequired 且必须 open

  Scenario: 三设备 append NotAppended 放弃全部 participant
    Given DeviceKey 1、2、3 prepare 成功且 append 返回 NotAppended
    When commit 完成明确未提交清理
    Then 返回 CommitFailed、State 和 RecordSeq 不变、只释放本地资源，未提交残留由权威恢复状态驱动 reconcile、transaction closed 且 gate 可重入

  Scenario Outline: 三设备 enqueue 故障返回 RecoveryRequired
    Given Delta 已 durable 并发布且 <label> 对应 DeviceKey <key> 的 enqueue_retirements 失败
    When commit 返回
    Then 返回 Ok(CommitResult)、published State 保持、三个 participant 各 enqueue_retirements 一次、诊断记录 DeviceKey=<key>、status=RecoveryRequired 且 gate released

    Examples:
      | label | key |
      | index | 1 |
      | hot | 2 |
      | cold | 3 |

  Scenario Outline: 三设备 enqueue 故障由 reopen 补偿
    Given <label> 对应 DeviceKey <key> 的 retirement 未成功登记且实例已围栏
    When 重新 open
    Then open 重新登记 retirement 并返回 Ready

    Examples:
      | label | key |
      | index | 1 |
      | hot | 2 |
      | cold | 3 |

  Scenario: cold 离线失败不保留部分运行时资源
    Given registry 固定 index=1、hot=2、cold=3 且 cold resolve 失败
    When open 返回 RecoveryFailed
    Then 已取得的 index 和 hot backend 资源释放且不返回 device map、snapshot lease 或 Superblock

  @critical
  Scenario: release 前旧 snapshot 在新提交后继续读取
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 旧 snapshot 在 release 前固定 index、hot、cold State
    When 新事务调用 Transaction::release 后调用 Transaction::commit 持久化并发布 remove 旧数据的 State
    Then 旧 snapshot 仍读取原数据且新 snapshot 拒绝已 removed BlockRef

  Scenario: 受保护对象不阻塞同步登记
    Given hot 有限回收轮遇到旧 snapshot lease 保护的对象
    When 新 commit 同步 enqueue_retirements
    Then enqueue 仅同步登记去重；有限轮计 guarded 并保留责任，不等待 lease，后续由外部调度 await 新一轮

  Scenario: 有限轮单对象删除失败不设置恢复围栏
    Given reclaim_retirements 已取得删除责任但单对象 unlink 失败
    When 有限轮计 retryable 并保留责任
    Then SuperblockStatus 不变且外部可调度后续有限轮；Unknown 仍须恢复裁决，不盲删

  @critical
  Scenario: cold 离线使整体 reopen 返回完整 OpenError
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given registry 固定 index=1、hot=2、cold=3 且 cold resolver 返回 source error
    When 重新 open
    Then 返回 OpenError::RecoveryFailed、kind=ResolveDevice、保留 cold DeviceKey 和 source 且不返回部分实例
