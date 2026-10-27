# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: RG-01, RG-02, RG-03, RG-04, RG-05, RG-06, RG-07, RG-08, RG-09, RG-10, RG-11, RG-12, RG-13, RG-14, RG-15, RG-16
Feature: 静态注册表与能力校验
  作为超级块创建者
  我希望设备注册信息满足稳定身份和容量边界
  以便 open 可以逐字段验证物理后端

  @critical
  Scenario: 注册表拒绝零设备
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given create 收到空的 InitialDevice 列表
    When create 校验静态 registry
    Then 返回 InvalidConfiguration

  Scenario: 注册表接受一个设备
    Given create 收到一个合法 InitialDevice
    When create 建立静态 registry
    Then registry 包含 DeviceKey 1 且 create 继续初始化

  Scenario: 注册表接受一百万个设备
    Given create 收到一百万个合法且唯一的 InitialDevice
    When create 校验设备数量
    Then 数量校验成功且最大 DeviceKey 为 1000000

  @critical
  Scenario: 注册表拒绝超过一百万个设备
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given create 收到 1000001 个 InitialDevice
    When create 校验设备数量
    Then 返回 InvalidConfiguration

  Scenario: DeviceRegistration 拒绝零 BlockDeviceId
    Given 一个 InitialDevice 的 BlockDeviceId 全为零
    When create 校验设备身份
    Then 返回 InvalidConfiguration

  Scenario: DeviceLabel 使用原始 UTF-8 字节
    Given 一个合法 DeviceLabel 包含多字节 UTF-8 字符
    When create 保存并 open 恢复注册表
    Then label 按原始 UTF-8 字节恢复且不执行规范化

  Scenario Outline: DeviceLabel 长度边界精确校验
    Given 一个 DeviceLabel 的 UTF-8 字节长度为 <length>
    When create 校验 label
    Then 校验结果为 <result>

    Examples:
      | length | result |
      | 0 | InvalidConfiguration |
      | 1 | accepted |
      | 255 | accepted |
      | 256 | InvalidConfiguration |

  @critical
  Scenario: DeviceLabel 重复时拒绝注册表
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 两个设备使用相同且大小写敏感的 DeviceLabel
    When create 校验静态 registry
    Then 返回 InvalidConfiguration

  Scenario: BlockDeviceId 重复时拒绝注册表
    Given 两个设备使用相同的非零 BlockDeviceId
    When create 校验静态 registry
    Then 返回 InvalidConfiguration

  Scenario: hot 和 Hot label 可以共存
    Given 两个设备的 DeviceLabel 分别为 hot 和 Hot
    When create 校验静态 registry
    Then registry 校验成功

  Scenario: DeviceKey 按输入顺序稳定分配
    Given create 按 index、hot、cold 顺序收到三个设备
    When create 分配 DeviceKey
    Then 三个设备依次获得 DeviceKey 1、2、3

  Scenario: 静态设备表禁止动态变更
    Given create 已建立按 DeviceKey 连续排列的 DeviceSlot Vec
    When 尝试注册、删除、替换或重编号设备
    Then 操作不存在或被拒绝，Vec 长度和 DeviceKey 到 slot 的 checked index 映射保持不变，且原有 Arc backend 不被替换

  @critical
  Scenario Outline: create 能力字段不匹配返回 NotCreated
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given DeviceKey <key> 的 initial backend 仅有 <field> 不匹配且其他能力合法
    When create 校验 capabilities
    Then 返回 CreateError::NotCreated、kind=IncompatibleDeviceCapabilities、保留 DeviceKey <key> 和 source

    Examples:
      | key | field |
      | 1 | protocol version |
      | 2 | base size |
      | 3 | exact level table |
      | 1 | independent large block |

  Scenario Outline: open 能力字段不匹配返回 RecoveryFailed
    Given DeviceKey <key> 的 resolved backend 仅有 <field> 不匹配且身份和其他能力合法
    When open 校验 capabilities
    Then 返回 OpenError::RecoveryFailed、kind=IncompatibleDeviceCapabilities、保留 DeviceKey <key> 和 source

    Examples:
      | key | field |
      | 1 | protocol version |
      | 2 | base size |
      | 3 | exact level table |
      | 1 | independent large block |

  Scenario Outline: optional capability flags 被 S3 判定兼容时继续
    Given DeviceKey <key> 的 backend 仅有 optional capability flags 与 registry 不同且 S3裁决 OPTIONAL-CAPABILITY=固定字段精确相等且optional flags满足(actual & required)==required 判定该差异兼容
    When <operation> 校验 capabilities
    Then capability 校验继续且不返回 IncompatibleDeviceCapabilities

    Examples:
      | operation | key |
      | create | 1 |
      | open | 2 |

  Scenario Outline: optional capability flags 被 S3 判定不兼容时保持外层错误
    Given DeviceKey <key> 的 backend 仅有 optional capability flags 与 registry 不同且 S3裁决 OPTIONAL-CAPABILITY=固定字段精确相等且optional flags满足(actual & required)==required 判定该差异不兼容
    When <operation> 校验 capabilities
    Then 当 operation=create 时返回 CreateError::NotCreated、kind=IncompatibleDeviceCapabilities、保留 DeviceKey <key> 和 source；当 operation=open 时返回 OpenError::RecoveryFailed、kind=IncompatibleDeviceCapabilities、保留 DeviceKey <key> 和 source

    Examples:
      | operation | key |
      | create | 1 | CreateError::NotCreated |
      | open | 2 | OpenError::RecoveryFailed |
