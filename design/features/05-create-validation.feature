# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: CR-01, CR-02, CR-03, CR-04, CR-05, CR-06, CR-07, CR-08, CR-09, CR-10, CR-11, CR-12
Feature: create 校验与取消
  作为初始化调用方
  我希望 create 的失败 kind 和取消状态精确
  以便只有已知未创建结果可以安全修正重试

  Scenario Outline: create 失败 source Some 矩阵
    Given create 在 <stage> 收到底层 source error
    When create 返回
    Then 返回 CreateError::NotCreated、kind=<kind> 且 source=Some(error)

    Examples:
      | stage | kind |
      | inspect | DeviceInspection |
      | codec | ApplicationCodec |
      | visitor | ApplicationValidation |
      | 日志准备 | LogPreparation |
      | append 前 | AppendNotStarted |

  Scenario Outline: create 失败 source None 矩阵
    Given create 在 <condition> 发现无底层 source 的验证失败
    When create 返回
    Then 返回 CreateError::NotCreated、kind=<kind> 且 source=None

    Examples:
      | condition | kind |
      | 配置非法 | InvalidConfiguration |
      | 设备非空 | DeviceNotEmpty |
      | 记录超过上限 | RecordTooLarge |

  @critical
  Scenario: create 不调用 backend begin_write_batch
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given create 收到合法静态设备和初始 application state
    When create 追加首个 checkpoint
    Then create 不调用任何 backend begin_write_batch、write 或 prepare

  Scenario: create 首 checkpoint 编码完整初始 State
    Given create 已通过设备、codec 和 visitor 校验
    When create 编码 sequence 1 checkpoint
    Then append-log envelope block_seq 为 1，superblock 从其注入 RecordSeq=1，8 字节 header 不保存 sequence、application metadata 或 body length，body 依次包含静态 slot registry、每设备 BlockDeviceState、空 roots、空 bindings 和 application payload

  Scenario: create 为每个设备建立十五个虚拟 Active
    Given create 有 N 个合法初始设备
    When 构造初始 BlockDeviceState
    Then 每设备恰有 15 个 min_version=1、唯一 Some 尾项且 allocation_runs 和 released_at 为空的虚拟 Active，large entries 为空且 next_id 为首个合法 ID，并且不创建或 flush 物理 segment

  @critical
  Scenario Outline: create future 各阶段取消要求先 open
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given create future 在 <stage> 发生取消
    When 调用方收到取消结果
    Then 调用方必须先调用 open 再决定是否重新 create

    Examples:
      | stage |
      | inspect |
      | codec |
      | visitor |
      | log preparation |
      | append 开始前 |
      | append 开始后 |

  Scenario: append 开始后的 create 取消要求 open 裁决
    Given create append 已开始且随后 future 被取消
    When 调用方观察 create future 已取消
    Then create 不返回 CreateError 且调用方必须先 open

  Scenario: append 未开始的 create 取消要求 open 裁决
    Given create 在 append 开始前发生可证明取消
    When 调用方观察 create future 已取消
    Then create 不返回 CreateError 且调用方必须先 open

  Scenario: create append Unknown 同步失败
    Given create 的同步 append 返回 DurableAppendOutcome::Unknown
    When create 裁决同步 append 结果
    Then 返回 CreateError::StatusUnknown 且保留 source

  @critical
  Scenario: create 只使用 InitialDevice 中的 Arc backend
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given create 收到合法 InitialDevice 列表和一个会失败的 resolver
    When create 完成 inspect、capability 校验和初始 checkpoint
    Then 所有设备操作只使用 InitialDevice.device 的 Arc<D>、resolver 调用次数为零且 resolver 的缺失映射或失败不影响 create 结果

  @critical
  Scenario: create 取消后 open 返回 NotInitialized 允许重新 create
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given create future 在任一阶段取消且调用方没有直接重试 create
    When open 清理可证明的临时状态并返回 OpenError::NotInitialized 后调用方使用相同合法配置重新调用 create
    Then create 进入正常校验并可成功写入唯一初始 checkpoint、sequence 从 1 开始、不继承已取消 create 的 reservation、临时对象、participant 或 writer gate，且不产生重复 registry、重复 checkpoint 或重复物理初始化

  @critical
  Scenario: create 取消后 open 返回 RecoveryFailed 禁止重新 create 覆盖介质
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given create future 已取消且 open 返回 OpenError::RecoveryFailed 并保留具体 kind 和 source
    When 调用方尝试以相同或不同配置重新 create
    Then 外层返回 CreateError，返回 CreateError::NotCreated { kind: LogPreparation, source: Some(original recovery evidence/error) }，且不得把介质视为空、不得执行日志初始化、truncate、replace、append 或新 checkpoint 写入、不得删除 preserved object、未知 append 结果或可恢复日志、不得调用会覆盖或清理已有介质事实的 backend 操作且原 RecoveryFailed 恢复证据保持不变
