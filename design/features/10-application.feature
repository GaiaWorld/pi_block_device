# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: AP-01, AP-02, AP-03, AP-04, AP-05, AP-06, AP-07, AP-08, AP-09, AP-10, AP-11, AP-12, AP-13, AP-14, AP-15, AP-16, AP-17, AP-18, AP-19, AP-20, AP-21, AP-22, AP-23, AP-24, AP-25
Feature: Application state、codec 与 context
  # ApplicationState::clone 必须由应用自行保证廉价；系统结构共享不改变应用状态的 clone 成本。
  # ApplicationApplyContext roots/bindings 使用 rpds 1.1.0 RedBlackTreeMapSync 借用，查询次序不变。
  # 拆分 DeviceTable 的 device_state 查询当前为 todo；原测试保留待迁移。
  作为应用状态实现者
  我希望 codec 有界、格式固定且 context 只提供逻辑前视图
  以便 application replay 可确定且不越权

  Scenario Outline: State 编码大小边界
    Given 一个编码后大小为 <size> 的 ApplicationState
    When ApplicationCodec 编码 State
    Then 结果为 <result>

    Examples:
      | size | result |
      | 64 MiB-1 | success |
      | 64 MiB | success |
      | 64 MiB+1 | RecordTooLarge |

  Scenario Outline: Delta 编码大小边界
    Given 一个编码后大小为 <size> 的 ApplicationDelta
    When ApplicationCodec 编码 Delta
    Then 结果为 <result>

    Examples:
      | size | result |
      | 16 MiB-1 | success |
      | 16 MiB | success |
      | 16 MiB+1 | RecordTooLarge |

  Scenario: application payload 自己解释 format metadata
    Given ApplicationCodec 需要 format_id 或 version
    When codec 编码和解码 application state payload
    Then metadata 由 application payload 自己编码、识别和校验，payload roundtrip 保留 metadata，且不进入通用 superblock header

  Scenario: open 映射 Delta codec mismatch
    Given checkpoint format 和 version 匹配但后续 Delta codec payload 无法解码
    When open replay 该 Delta
    Then 返回 OpenError::RecoveryFailed、kind=RecoveryFailureKind::Decode 且保留 codec source

  Scenario: Empty codec 单元拒绝非空 state
    Given EmptyApplicationCodec 只接受零长度 state bytes
    When codec 单元解码非空 state bytes
    Then 返回 ApplicationCodecError 且不产生 ApplicationState

  Scenario: create 映射 Empty codec 非空 state
    Given EmptyApplicationCodec 编码初始 state 得到非空 bytes
    When create 编码初始 checkpoint
    Then 返回 CreateError::NotCreated 且 kind=ApplicationCodec 并保留 source

  Scenario: open 映射 Empty codec 非空 state
    Given checkpoint 为 EmptyApplicationCodec 携带非空 application state bytes
    When open 解码 checkpoint
    Then 返回 OpenError::RecoveryFailed 且 kind=Decode 并保留 source

  Scenario: ApplicationApplyContext 只包含此前逻辑变化
    Given System A、Application X、System B 按顺序进入 transaction
    When Application X 的 apply 读取 context
    Then context 包含 A 的逻辑结果且不包含 B、physical write 或 prepare

  @critical
  Scenario: visitor 重复引用去重后验证全部持久 BlockRef
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given application state 的 visitor 输出 B1、B1、B2
    When 调用 visit_block_refs
    Then 超级块以 BTreeSet 去重并各验证一次 B1 和 B2

  Scenario: visitor 非 live 引用阻止 commit
    Given application visitor 输出一个不属于 candidate live 集合的 BlockRef
    When commit 执行 visitor validation
    Then 返回 SuperblockError::InvalidBlockRef、candidate 不发布且 backend write 和 prepare 不调用；此 commit 合同待实现，当前方法仅为 todo 占位

  Scenario: visitor 不触发 GC 或自动 release
    Given application visitor 只输出当前持久 BlockRef 集合
    When commit 完成 visitor validation
    Then 不生成 ReleaseSegmentBlocks、ReleaseLargeBlock、RemoveSegment 或 RemoveLargeBlock

  Scenario: application apply 失败不发布 candidate
    Given ApplicationDelta 在 change_index 2 返回 ApplicationApplyError
    When replay Delta
    Then 返回 ApplicationApplyError 且已发布 State 不变

  Scenario: visitor 成功完成外层校验
    Given application visitor 访问的每个 BlockRef 都属于固定 State 的 live 集合
    When commit 执行 visitor validation
    Then 外层 transaction API 返回成功且 candidate 可继续校验

  Scenario: visitor 错误映射 ApplicationVisit
    Given application visitor 在访问期间返回 source error
    When commit 执行 visitor validation
    Then 外层返回 SuperblockError 且 kind=ApplicationVisit 并保留 source

  @critical
  Scenario Outline: ApplicationEncoder 不暴露可绕过 hard_limit 的可变入口
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given crate 外部调用方持有 ApplicationEncoder
    When 编译尝试使用 <entry>
    Then 编译失败且底层 BytesMut 保持私有

    Examples:
      | entry |
      | DerefMut |
      | BufMut |
      | chunk_mut |
      | 底层可变 buffer getter |

  Scenario Outline: ApplicationEncoder 每个 put 越界前原子失败
    Given encoder 当前长度为 hard_limit-N 且 <put> 需要写入 M 字节并满足 M>N
    When 调用该 checked put 方法
    Then 返回 ApplicationCodecError、buffer 长度和原有 bytes 完全不变且 remaining 不变

    Examples:
      | put |
      | put_slice |
      | put_u8 |
      | put_u16_le |
      | put_u32_le |
      | put_u64_le |

  Scenario Outline: ApplicationEncoder 成功 put 按实际字节递减 remaining
    Given 新 ApplicationEncoder 的 hard_limit 为 L 且初始 remaining=L
    When 依次调用 <put> 写入 <bytes> 字节
    Then 每次成功后 remaining 按实际写入字节减少 <bytes>、buffer 只包含已写入 bytes 且没有容量垃圾

    Examples:
      | put | bytes |
      | put_slice | N |
      | put_u8 | 1 |
      | put_u16_le | 2 |
      | put_u32_le | 4 |
      | put_u64_le | 8 |

  @critical
  Scenario: ApplicationEncoder freeze 返回已写 bytes 并消费 self
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given ApplicationEncoder 已成功写入 payload P 且 remaining 大于零
    When 调用 freeze
    Then 只返回精确的 payload P、不包含容量垃圾、encoder 被消费且不再拥有可写状态

  Scenario: ApplicationEncoder freeze 后不能再次使用
    Given ApplicationEncoder 已被 freeze 消费
    When 编译尝试再次调用 put 或 freeze
    Then 编译失败
