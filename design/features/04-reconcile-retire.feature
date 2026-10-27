# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: RC-01, RC-02, RC-03, RC-04, RC-05, RC-06, RC-07, RC-08, RC-09, RC-10, RC-11, RC-12, RC-13, RC-14, RC-15, RC-16, RC-17, RC-18, RC-19, RC-20, RC-21, RC-22
Feature: 文件 reconcile 与 retire
  # 文件后端成功 media_metadata_synced=true 已有真实 I/O 验收；以下上层恢复场景不代表 Superblock 端到端验收。
  # 2026-10-08 用户收窄故障模型：排除突然掉电导致新文件名、rename/remove 目录项未落盘；不是原合同早已如此。
  # media_metadata_synced=true 表示当前模型所需操作及实际文件 sync 完成，不表示父目录 fsync；错误、取消和进程退出恢复仍考虑。
  # required 对象验证、Active 权威边界外尾部安全截断及 flush、孤儿清理、required 文件和 manifest sync 均完成后才能成功。
  # 文件有限异步回收已实现，旧线程移除；commit 登记未接入，Superblock::open/checkpoint/状态表仍占位，端到端重启补偿未完成。
  # public 单轮真实 I/O 测试迁移完成；取消动态证据仅覆盖删除准入后、底层提交前，不宣称底层已提交删除取消测试完成。
  # 取得执行资格时确定本轮对象范围，后续新入队留下一轮；仅长度 budget 不保证范围。每个选定对象最多尝试一次，Guarded/单对象失败保留且继续后续，失败计 retryable。
  # 成功整轮 RetirementReport 四项均 u64，attempted=completed+guarded+retryable；completed 含已确认不存在，不是物理删除数，报告不是队列余额。
  # 无法安全继续的内部错误才 DeviceError，Err 前完成不撤销且无整轮统计可依赖；取消保留未确认责任，底层已提交删除可继续，下轮按证据确认，Unknown 不盲完成或盲删。
  # lease 注册与删除并发安全：已有 lease 不得删除，删除已开始后不得授予对应对象不安全的新 lease；不指定锁算法，不改变 durable 事务终态，内存队列不是权威持久状态。
  作为恢复和物理回收实现者
  我希望介质修复、进度报告和延迟删除可独立验证
  以便 open 可安全重试并保护旧读取

  Scenario: 虚拟空 Active 缺少文件时成功 reconcile
    Given 期望状态包含空的虚拟 Active 且介质没有对应文件
    When open 执行 reconcile
    Then reconcile 成功且不物化该空 Active

  Scenario: 零长空段孤儿被删除
    Given 介质存在不属于期望状态的零长空段
    When reconcile 执行孤儿计划
    Then 该孤儿被删除且 remaining_actions 为零

  @critical
  Scenario: 非空 Active 缺失返回 RequiredObjectMissing
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 期望状态引用一个非空 Active 且对应文件缺失
    When open 执行 reconcile
    Then ReconcileError.kind 为 RequiredObjectMissing 且保留 source

  Scenario: 非空 Active 损坏返回 RequiredObjectCorrupt
    Given 期望状态引用一个非空 Active 且对应文件损坏
    When open 执行 reconcile
    Then ReconcileError.kind 为 RequiredObjectCorrupt 且保留 source

  Scenario: Sealed 缺失返回 RequiredObjectMissing
    Given 期望状态引用一个 Sealed 且对应文件缺失
    When open 执行 reconcile
    Then ReconcileError.kind 为 RequiredObjectMissing 且保留 source

  @critical
  Scenario: Sealed 损坏返回 RequiredObjectCorrupt
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 期望状态引用一个 Sealed 且对应文件损坏
    When open 执行 reconcile
    Then ReconcileError.kind 为 RequiredObjectCorrupt 且保留 source

  Scenario: reconcile 成功进度满足完整不变量
    Given reconcile 计划 P 个动作并完成全部动作
    When reconcile 返回 ReconcileReport
    Then completed_actions=P、remaining_actions=0、partial_progress=false 且 media_metadata_synced=true

  Scenario: 未提交 Active 半追加尾部按权威 State 恢复
    Given required Active 保留完整权威前缀但含未提交多余尾部且没有现存 lease 或 mapping 保护
    When open 恢复阶段执行 reconcile
    Then 仅截断权威分配边界之外的尾部并实际同步文件，保留前缀字节且重复恢复幂等
    And required 前缀不足或 Sealed/large 长度不符仍返回 RequiredObjectCorrupt，不重建或补零

  Scenario: RepairIo 失败报告完整进度和 source
    Given reconcile 计划 P 个动作并在完成 C 个动作后发生 repair I/O 错误
    When reconcile 返回 ReconcileError
    Then kind=RepairIo、completed_actions=C、remaining_actions=P-C、partial_progress=(C>0)、media_metadata_synced=false 且保留 source

  Scenario: MediaMetadataSync 失败不计入 completed
    Given reconcile 已完成 C 个 durable 动作且下一动作的数据写入完成但介质元数据同步失败
    When reconcile 返回 ReconcileError
    Then kind=MediaMetadataSync、completed_actions=C、remaining_actions=P-C、partial_progress=(C>0)、media_metadata_synced=false 且保留 source

  Scenario: 部分完成失败满足 progress 数值不变量
    Given reconcile 计划 P 个动作并完成 C 个 durable 动作后失败且 0<C<P
    When reconcile 返回 ReconcileError
    Then completed_actions=C、remaining_actions=P-C、partial_progress=true 且保留 source

  Scenario: 并发介质变更返回 ConcurrentMutation
    Given reconcile 读取期望对象后介质被外部修改
    When reconcile 执行带版本检查的动作
    Then ReconcileError.kind 为 ConcurrentMutation 且保留 source

  @critical
  Scenario: enqueue 失败使 durable commit 进入恢复围栏
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given durable remove 已产生一个 RetiredObject
    When enqueue_retirements 写入责任登记失败
    Then commit 返回 Ok(CommitResult) 且 status=RecoveryRequired 并保留 DeviceError source

  Scenario: enqueue 仅登记去重而不启动任务
    Given durable remove 已产生多个 RetiredObject
    When enqueue_retirements 成功登记
    Then 调用同步返回且仅登记去重不启动任务，外部定时 await reclaim_retirements 执行一次有限轮
    And 重叠调用内部异步串行化且不暴露 token，取得资格后空轮全零立即返回，无 sleep、timer、spawn 或空队列等待

  @critical
  Scenario: 有限回收轮保留旧 snapshot 保护的对象
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 旧 snapshot 或 ReadBuf 仍保护一个 RetiredObject
    When 外部 await reclaim_retirements 尝试处理该对象
    Then 本轮计 guarded 并保留责任且继续后续对象，文件继续存在直到保护资源全部释放

  Scenario: enqueue 失败由重启补偿
    Given 业务 durable remove 成功但 enqueue_retirements 返回错误
    When 随后重新 open
    Then open 重新登记未完成的 retirement 且返回 Ready

  Scenario: reconcile 失败后从同一 expected 重新规划
    Given 首次 reconcile 对 expected State E 部分完成后失败
    When 使用同一 expected State E 再次 reconcile
    Then 从 E 和当前介质重新生成完整计划且重复动作幂等

  Scenario Outline: open 可重试 reconcile 在第 1、2 或 3 次成功后立即停止
    Given 同一 expected State E 在成功前依次返回 <retry_kinds>，且每个 kind 均被 S3裁决 RECONCILE-RETRY=仅三类瞬态最多三次且取消不计次不续跑 分类为幂等瞬态可重试
    When open 按首版内部默认配置执行 reconcile 并在第 <success_attempt> 次成功
    Then open 返回完整实例、reconcile 总调用次数恰为 <success_attempt>、成功后不执行额外尝试、每次均从 E 和当次实际介质从头生成完整计划、不把 progress 作为续跑游标，且最多三次是首版内部默认而不是公共 API 承诺

    Examples:
      | retry_kinds | success_attempt |
      | 无 | 1 |
      | RepairIo | 2 |
      | MediaMetadataSync | 2 |
      | ConcurrentMutation | 2 |
      | RepairIo、MediaMetadataSync | 3 |
      | MediaMetadataSync、ConcurrentMutation | 3 |
      | ConcurrentMutation、RepairIo | 3 |

  Scenario: open 首版默认三次可重试 reconcile 均失败后停止
    Given 同一 expected State E 的三次 reconcile 均返回 S3裁决 RECONCILE-RETRY=仅三类瞬态最多三次且取消不计次不续跑 分类的可重试错误
    When open 完成首版内部默认配置的第三次尝试
    Then 返回 OpenError::RecoveryFailed、kind=Reconcile、source 来自最后一次尝试、不执行第四次 reconcile，且每次均从 E 和当次实际介质从头生成完整计划

  Scenario: 虚拟空 Active 成功报告完整 progress
    Given expected 仅缺少一个虚拟空 Active 的物理对象
    When reconcile 成功
    Then inspected_objects 已记录、planned_actions=0、completed_actions=0、remaining_actions=0、partial_progress=false 且 media_metadata_synced=true

  Scenario Outline: 首次物化失败残留由权威 expected State 裁决
    Given 首次普通块物化在 <point> 因 <outcome> 中断且 expected State 仍把该 Active 视为虚拟空 Active
    When open 从 expected State 和实际介质重新规划 reconcile
    Then 缺少 segment 合法、临时或零长孤儿被删除、不得把残留提升为已物化 segment 且 reconcile 可幂等重试

    Examples:
      | point | outcome |
      | 创建 segment 后 | DeviceError |
      | 创建 segment 后 | future 取消 |
      | 首 write_at 部分写后 | DeviceError |
      | 首 write_at 部分写后 | future 取消 |
      | flush 前或期间 | DeviceError |
      | flush 前或期间 | future 取消 |

  Scenario Outline: required object reconcile 错误立即失败且不重建
    Given reconcile 对 expected State E 返回 <kind>
    When open 按 S3裁决 RECONCILE-RETRY=仅三类瞬态最多三次且取消不计次不续跑 分类该错误
    Then 立即返回 OpenError::RecoveryFailed、kind=Reconcile、保留首次 source、不执行第二次 reconcile 且不得重建、替换或提升 required object

    Examples:
      | kind |
      | RequiredObjectMissing |
      | RequiredObjectCorrupt |

  Scenario Outline: reconcile 取消按最终取消裁决停止且不自动续跑
    Given open 的第 <attempt> 次 reconcile 正在从同一 expected State E 和当前介质从头规划或执行
    When reconcile future 被取消
    Then 按 S3裁决 RECONCILE-RETRY=仅三类瞬态最多三次且取消不计次不续跑 的取消裁决停止、不把 progress 作为续跑游标、不自动开始下一次尝试，且后续显式 open 若获准重试仍从 E 和届时实际介质从头生成完整计划

    Examples:
      | attempt |
      | 1 |
      | 2 |
      | 3 |
