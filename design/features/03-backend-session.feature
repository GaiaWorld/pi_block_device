# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: BS-01, BS-02, BS-03, BS-04, BS-05, BS-06, BS-07, BS-08, BS-09, BS-10, BS-11, BS-12, BS-13, BS-14, BS-29, BS-30, BS-15, BS-16, BS-17, BS-18, BS-19, BS-20, BS-21, BS-22, BS-23, BS-24, BS-25, BS-26, BS-28, BS-27
# 文件后端 write/prepare 已有真实 I/O 验收；Superblock 受影响运行时仍占位，不代表端到端通过或目录 fsync 已实现。
# 2026-10-08 用户变更故障模型：排除突然掉电导致新文件名、rename/remove 目录项未落盘；不是原合同早已如此。
# prepare(self) 消费批次；成功时数据完整、最终布局可恢复定位、必要文件 DataAndMetadata 同步真实成功先于 metadata durable。
# 创建发布删除错误、原子 noreplace、取消和进程退出恢复仍在范围内，不以文件 flush 冒充父目录 fsync。
# Drop 仅释放资源，不删除可能已提交的数据；未提交残留由权威恢复状态驱动 reconcile，Unknown 由 pi_superblock 恢复裁决。
# 旧规则 ID 映射及旧测试待迁移，旧三终态规则已失效。
Feature: Backend 与 write batch 准备合同
  作为后端实现者
  我希望 batch 资源和 prepare 具有线性所有权
  以便取消、异常和 append 未知结果不会留下未定义资源

  @critical
  Scenario: backend 异步 future 满足 Send
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 一个实现 BlockDeviceBackend 的类型
    When 编译其 RPITIT 异步方法
    Then 返回 future 满足 Send 且 backend 满足 Send、Sync 和 'static

  @critical
  Scenario: begin_write_batch 同步创建空批次且不占用写锁
    Given 一个未被恢复围栏拒绝的 backend
    When 调用 begin_write_batch(self: &Arc<Self>)
    Then 同步返回 Result<WriteBatch,DeviceError>，不执行 I/O 或等待写锁，不创建持久对象、marker、queue 或 reservation

  Scenario: 文件批次首次 write 等待取消不产生恢复围栏
    Given 一个批次已首次 write 并持有 write gate，另一个空批次同步 begin 成功
    When 取消另一个批次首次 write 的锁等待 future
    Then 不发起 I/O、不设置恢复围栏、不保留锁预约，原批次释放后可再次 write

  Scenario: 未写批次不阻塞非空回收
    Given 文件后端持有一个尚未 write 的空批次且退休队列非空
    When await reclaim_retirements
    Then 不等待空批次释放，按既有 lease 与删除证据合同完成有限回收轮

  Scenario: Superblock begin_write 取消只释放 gate
    Given Superblock::begin_write 在返回 Transaction 前发生取消
    When 调用方取消 begin_write future
    Then writer gate 被释放且不存在 backend batch 或持久副作用

  Scenario: prepare 消费批次且完成可恢复布局
    Given write 已接收完整数据且批次尚未消费
    When prepare(self) 返回成功
    Then 数据完整、最终布局可恢复定位、必要文件 DataAndMetadata 同步真实成功先于 metadata durable，且同一批次不可再次 write 或 prepare

  Scenario: Drop 只释放资源
    Given 一个可能包含已提交数据或未提交残留的批次
    When 批次或 prepare future 被丢弃
    Then 仅释放资源，不删除可能已提交的数据；未提交残留由基于权威恢复状态的 reconcile 回收

  Scenario: 后端不裁决 Unknown
    Given metadata append 结果为 Unknown
    When pi_superblock 重新 open 恢复
    Then 由权威恢复状态裁决提交结果，后端不接收 TransactionId 或执行事务终态

  Scenario: inspect 对非空介质零修改并报告 empty false
    Given 后端目录包含 segment、large block、marker、queue 和 reservation
    When backend 单元调用 inspect
    Then InspectReport.empty=false 且目录对象、内容和介质元数据逐项不变

  Scenario: inspect 仅由 create 调用
    Given 一个已经 open 的 Superblock
    When 运行时尝试从 open 路径调用 inspect
    Then 该调用路径不存在

  Scenario Outline: inspect 对象类别统一报告非空
    Given 后端仅包含一个 <object_kind> 对象
    When create 调用 inspect
    Then InspectReport.empty 为 false 且 create 返回外层 CreateError::NotCreated、kind 为 DeviceNotEmpty、source=None

    Examples:
      | object_kind |
      | segment |
      | large block |
      | temporary object |
      | preserved object |
      | retirement queue entry |
      | unknown object |

  Scenario: inspect 空后允许 create 继续
    Given 后端不包含任何对象类别
    When create 调用 inspect
    Then InspectReport.empty 为 true 且 create 不返回 DeviceNotEmpty

  Scenario Outline: BlockWritePosition 和 BlockWriteChunk checked 边界及 roundtrip
    Given BlockWritePosition::new(<offset>,<logical_len>) 与三参数 BlockWriteChunk::new(B,<chunk_len> 字节 chunk,position) 按该输入执行
    When checked 构造结果为 <construction>
    Then <assertion>

    Examples:
      | offset | logical_len | chunk_len | construction | assertion |
      | 0 | 8192 | 4096 | success | batch write 仅消费一个 request；offset=0、logical_len=8192，is_last=false，未写范围保持为零 ；单个分片不代表完整数据，prepare 成功前必须写满逻辑范围 |
      | 4096 | 8192 | 4096 | success | batch write 仅消费一个 request；offset=4096、logical_len=8192，is_last=true，未写范围保持为零 ；单个分片不代表完整数据，prepare 成功前必须写满逻辑范围 |
      | 8193 | 8192 | 1 | position failure | batch、backend 和 write 均不调用 |
      | 0 | 0 | 1 | position failure | batch、backend 和 write 均不调用 |
      | 0 | 8192 | 0 | chunk failure | batch、backend 和 write 均不调用 |
      | 0 | 8192 | 8193 | chunk failure | batch、backend 和 write 均不调用 |
      | 8190 | 8192 | 3 | chunk failure | batch、backend 和 write 均不调用 |
      | 18446744073709551615 | 18446744073709551615 | 1 | chunk failure | checked offset+chunk.len 溢出，batch、backend 和 write 均不调用 |

  Scenario: 后端只接收单个 checked chunk
    Given 一个 BlockDeviceWriteBatch
    When 查看写入协议
    Then 只有 write(&mut self, BlockWriteChunk) 和 prepare(self) 的 Send future，不提供多段或 stream 方法，也无 PreparedBatch token

  @critical
  Scenario: 文件后端 create 生成并持久化稳定身份
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 一个不存在的文件后端目录和合法 FileBlockDeviceConfig
    When FileBlockDevice::create 完成
    Then 返回可用 FileBlockDevice、manifest 持久化非零 BlockDeviceId 且再次 create 拒绝并不覆盖现有介质

  @critical
  Scenario: 文件后端 open 恢复身份且保持非破坏
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 一个含合法 manifest、BlockDeviceId 和已有数据对象的文件后端目录
    When FileBlockDevice::open 完成或返回错误或 future 被取消
    Then 成功时 device_id 等于 manifest 身份且所有路径均不创建、截断、重写或删除已有介质

  @critical
  Scenario Outline: 文件后端 create 和 open 拒绝非法配置或路径且零修改
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 文件后端调用 <operation> 使用 <invalid_input> 且记录调用前路径快照
    When 执行 FileBlockDevice::<operation>
    Then 返回 FileDeviceError::<error> 且路径集合、文件内容、长度和介质元数据逐项不变

    Examples:
      | operation | invalid_input | error |
      | create | 空 root | InvalidConfiguration |
      | create | 相对 root | InvalidConfiguration |
      | create | 已存在目标 | AlreadyExists |
      | open | 空 root | InvalidConfiguration |
      | open | 相对 root | InvalidConfiguration |
      | open | 不存在目标 | NotFound |

  Scenario: 文件后端 create 成功写入固定格式 manifest
    Given 一个合法且不存在的绝对 root
    When FileBlockDevice::create 成功
    Then manifest 原子持久化 format_version、固定 protocol_version、非零 BlockDeviceId、base_block_size、十五项 level_sizes 和 independent_large_blocks=true，且返回 capabilities 等于 BackendCapabilities::fixed()

  @critical
  Scenario Outline: 文件后端 open 拒绝非法 manifest 且不修复
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given root 含 <invalid_manifest> 且记录全部介质字节和目录项
    When FileBlockDevice::open 使用 expected_device_id=None
    Then 返回 FileDeviceError::InvalidManifest 且 manifest、数据对象和目录项逐字节不变

    Examples:
      | invalid_manifest |
      | magic 错误 |
      | format_version 不支持 |
      | protocol_version 不匹配 |
      | BlockDeviceId 全零 |
      | base_block_size 或 level_sizes 不匹配固定协议 |
      | independent_large_blocks=false |
      | 截断或带尾随字节 |

  @critical
  Scenario: 文件后端 open 拒绝 caller expected identity mismatch
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given root 含合法 manifest 身份 A 和已有数据对象且 caller expected_device_id=B
    When FileBlockDevice::open 完成 manifest 格式验证后比较身份
    Then 返回 FileDeviceError::IdentityMismatch 且不创建、截断、重写、删除或修复任何介质对象

  Scenario: 文件后端 open 接受匹配或未指定 expected identity
    Given root 含合法固定格式 manifest 身份 A
    When FileBlockDevice::open 分别使用 Some(A) 和 None
    Then 两次均返回 device_id=A 且 capabilities 等于 BackendCapabilities::fixed()

  @critical
  Scenario Outline: 文件后端 create 和 open 失败或取消保持调用前介质
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given <operation> 在 <point> 返回 I/O error 或 future 被取消且记录调用前快照
    When 调用结束或 future 被丢弃
    Then 既有路径、manifest、数据对象、内容和长度逐项不变，且未发布的 create 临时文件被清理或保持不可被 open 识别

    Examples:
      | operation | point |
      | create | manifest 临时文件写入前 |
      | create | manifest sync 前 |
      | create | 原子发布前 |
      | open | manifest 读取中 |
      | open | manifest 校验中 |
      | open | identity 比较前 |

  Scenario: 文件后端错误映射保持冻结
    Given create 或 open 遇到配置、存在性、manifest、identity 或其他文件系统错误
    When 映射为 FileDeviceError
    Then 精确映射依次为 InvalidConfiguration、AlreadyExists 或 NotFound、InvalidManifest、IdentityMismatch、Io source 且不新增隐式能力不匹配分支

  @critical
  Scenario: 文件后端能力固定且调用方不能配置
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 任意通过 create 或 open 返回的 FileBlockDevice
    When 查询 BlockDeviceBackend::capabilities 并检查 FileBlockDeviceConfig 公开字段
    Then capabilities 恒等于 BackendCapabilities::fixed() 且 config 只有 root、manifest 不持久化调用方可选 capabilities 或 optional flags
