# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: OP-01, OP-02, OP-03, OP-04, OP-05, OP-06, OP-07, OP-08, OP-09, OP-10, OP-11, OP-12, OP-13, OP-14, OP-15, OP-16, OP-17
Feature: open、resolver 与恢复裁决
  作为重启调用方
  我希望每个设备都被解析、身份校验并 reconcile
  以便 open 不返回部分实例

  @critical
  Scenario: resolver 失败只映射 ResolveDevice
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given resolver 无法为 DeviceKey 2 返回 backend
    When open 解析静态 registry
    Then 外层返回 RecoveryFailed 且 kind 为 ResolveDevice 并保留 source

  Scenario: backend 身份不匹配映射 DeviceIdentityMismatch
    Given resolver 返回 backend 但 BlockDeviceId 不同于注册表
    When open 校验设备身份
    Then 外层返回 RecoveryFailed 且 kind 为 DeviceIdentityMismatch 并保留 source

  Scenario: backend 能力不匹配映射 IncompatibleDeviceCapabilities
    Given resolver 返回 backend 但固定能力字段不同于注册表
    When open 校验 capabilities
    Then 外层返回 RecoveryFailed 且 kind 为 IncompatibleDeviceCapabilities 并保留 source

  Scenario: reconcile 错误映射 Reconcile
    Given resolver 成功且身份和能力匹配
    When backend reconcile 返回 ReconcileError
    Then 外层返回 RecoveryFailed 且 kind 为 Reconcile 并保留 source

  @critical
  Scenario: 全部设备在线后 open 才成功
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given registry 包含 index、hot、cold 三个设备
    When resolver、身份能力校验和 reconcile 全部成功
    Then open 返回包含三个设备的 Ready Superblock

  @critical
  Scenario: open 仅通过外部 resolver 映射 registry 设备位置
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given checkpoint registry 只含 DeviceKey、BlockDeviceId 和 DeviceLabel 且外部 resolver 保存位置映射
    When open 按 DeviceKey 解析全部设备
    Then 每个 backend 仅由 resolver 返回的 Arc<D> 提供且 open 不从 registry wire 读取 Path、URI 或 locator

  Scenario: 有效日志恢复返回 Ready
    Given 日志包含合法首 checkpoint 和严格递增的完整记录
    When open 解码、replay、解析并 reconcile
    Then open 返回 Ready Superblock

  Scenario: 空容器返回 NotInitialized
    Given 容器没有完整初始 checkpoint 且临时状态可安全清理
    When open 检查日志
    Then 外层返回 NotInitialized 且不返回 Superblock

  @critical
  Scenario: 损坏帧返回 Decode
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 日志存在无法解码的完整帧
    When open 尝试恢复
    Then 外层返回 RecoveryFailed 且 kind 为 Decode 并保留 source

  Scenario: 不支持格式返回 UnsupportedFormat
    Given 日志的 system version 不受当前版本支持
    When open 检查日志格式
    Then 外层返回 RecoveryFailed 且 kind 为 UnsupportedFormat 并保留 source

  Scenario: application 格式不匹配返回 ApplicationFormatMismatch
    Given checkpoint application payload 的自有 format_id 或 version 与当前 codec 不匹配
    When open 解码 checkpoint
    Then 外层返回 RecoveryFailed 且 kind 为 ApplicationFormatMismatch 并保留 source

  Scenario: application replay 失败返回 ApplicationReplay
    Given 日志 Delta 的 application change 无法被 apply
    When open replay Delta
    Then 外层返回 RecoveryFailed 且 kind 为 ApplicationReplay 并保留 source

  Scenario: reconcile 失败返回 ReconcileFailure source
    Given backend reconcile 返回带 kind、progress 和 source 的 ReconcileError
    When open 处理该错误
    Then 外层返回 RecoveryFailed 且 kind 为 Reconcile 并保留 source

  Scenario: 设备身份错误不映射为 resolver 错误
    Given resolver 已成功返回 backend 且 backend identity 校验失败
    When open 处理该设备
    Then 外层 kind 为 DeviceIdentityMismatch 而不是 ResolveDevice

  Scenario: 日志状态不可判定返回 LogStateIndeterminate
    Given append 边界无法证明日志是否完整
    When open 裁决日志状态
    Then 外层返回 RecoveryFailed 且 kind 为 LogStateIndeterminate 并保留 source

  Scenario: NotInitialized 后临时容器清理幂等
    Given open 返回 NotInitialized
    When 调用方清理临时状态后再次 open
    Then open 再次返回 NotInitialized 且没有残留临时对象

  Scenario Outline: open 设备阶段错误保留操作上下文且不返回部分实例
    Given DeviceKey <key> 在 <operation> 返回底层 source error 且此前设备已成功处理
    When open 处理该错误
    Then 返回 OpenError::RecoveryFailed、保留 operation=<operation>、DeviceKey=<key> 和 source，释放此前取得的资源且不返回部分 Superblock

    Examples:
      | operation | key |
      | resolve | 2 |
      | reconcile | 3 |
