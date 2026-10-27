# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: WF-01, WF-02, WF-03, WF-04, WF-05, WF-06, WF-07, WF-08, WF-09, WF-10, WF-11, WF-12, WF-13, WF-14, WF-15, WF-16, WF-17, WF-18, WF-19, WF-20, WF-21, WF-22, WF-23, WF-24, WF-25, WF-26, WF-27, WF-28, WF-29, WF-30, WF-31, WF-32, WF-33, WF-34, WF-35, WF-36, WF-37, WF-38, WF-39, WF-40, WF-41, WF-42
Feature: wire 格式与 golden 校验
  # 持久化集合调整不升级 system_version 或 wire 字段，保持原有严格键升序及 Vector 序号顺序编码。
  # DeviceTable 拆分不改变 checkpoint 内按一基 DeviceKey 对应的 registration/state 顺序。
  # 不启用 rpds serde。2026-10-12 checkpoint纯状态codec已实现并独立验证，不等于整个Feature/runtime完成。
  # 用户本切片授权沿用现有SuperblockError carrier；InternalValidationKind分类缺口保留，不新增variant。
  作为持久化格式维护者
  我希望每个字段偏移、顺序、限制和拒绝行为固定
  以便独立 fixture 可以验证实现

  @critical
  Scenario: PISB header 四字段位于冻结偏移
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 一个合法 PISB header
    When 编码 header
    Then magic、system_version、record_kind、flags 位于第15.2节规定偏移且总长为 8 字节

  Scenario: header flags 固定为零
    Given 一个待编码的合法 header
    When 编码 flags
    Then flags 字段为 0

  Scenario: 非零 header flags 映射 InvalidState
    Given 一个 flags 非零的 8 字节 header
    When decoder 校验 header
    Then 返回 InternalValidationKind::InvalidState 且不发布 State

  Scenario Outline: header 冻结字段非法值被严格拒绝
    Given 一个 8 字节 header 的 <field> 为 <value>
    When decoder 校验 header
    Then 返回 InternalValidationKind::InvalidState 且不发布 State

    Examples:
      | field | value |
      | magic | 非 PISB 四字节 |
       | system_version | 0 |
       | system_version | 2 |

  Scenario Outline: header record_kind 仅接受 Checkpoint 和 Delta
    Given header record_kind 为 <kind>
    When decoder 校验 record_kind
    Then 结果为 <result>

    Examples:
      | kind | result |
      | 1 | Checkpoint |
      | 2 | Delta |
      | 0 | InternalValidationKind::UnknownTag |
      | 3 | InternalValidationKind::UnknownTag |
      | 255 | InternalValidationKind::UnknownTag |

  Scenario Outline: append-log block_seq 严格转换为 RecordSeq
    Given append-log envelope 的 block_seq 为 <block_seq>
    When decoder 注入 superblock record sequence
    Then 结果为 <result>，且 RecordSeq 只由该 block_seq 注入，8 字节 header 和 body 均不再保存第二个 record sequence

    Examples:
      | block_seq | result |
      | 0 | InternalValidationKind::InvalidState |
      | 1 | success |
      | u32::MAX-1 | success |
      | u32::MAX | InternalValidationKind::InvalidState |
      | u32::MAX+1 | InternalValidationKind::InvalidState |

  Scenario: checkpoint 按冻结顺序编码完整 State
    Given State 包含静态 registry、15 levels、large blocks、roots、bindings 和 application
    When 编码 checkpoint body
    Then 字段顺序符合第15.3-15.4节，header 不含 application metadata 或 body length，bindings 后剩余 bytes 原样交给 application codec

  Scenario: registry count 一百万可编码
    Given checkpoint registry 包含 1000000 个设备
    When 编码 device_count
    Then 编码成功且 device_count=1000000

  Scenario: registry count 一百万零一被拒绝
    Given checkpoint registry 包含 1000001 个设备
    When 编码 device_count
    Then 编码被拒绝且不分配对应集合

  Scenario: 后续 checkpoint registry 字节等价
    Given 初始和后续 checkpoint 使用相同 key、ID、label 序列
    When 分别编码 registry
    Then 两段 registry bytes 完全相同

  @critical
  Scenario: registry wire 只持久化 key、ID、label 和 device State
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given registry 设备在应用配置中另有 Path、URI、locator 和 credential
    When 编码 checkpoint registry
    Then 每项仅编码 DeviceKey、BlockDeviceId、DeviceLabel 和 BlockDeviceState，wire 中不包含 Path、URI、locator 或 credential 字节

  Scenario: TransactionId 不进入 checkpoint 或 Delta wire
    Given runtime transaction 使用 pi_superblock 定义的 TransactionId，物理 backend 批次不接收该标识
    When 编码 checkpoint、System Delta 和 Application Delta
    Then 所有 wire 字段均不包含 TransactionId 且 recovery 不从 wire 重建 TransactionId

  Scenario Outline: segment SystemChange Delta item roundtrip
    Given 一个 <change> segment SystemChange
    When 编码并解码 Delta item
    Then transaction_tag=1、change_tag=<tag>、flags=0、payload_len 精确且 payload 为 <fields> 并 roundtrip

    Examples:
      | tag | change | fields |
      | 10 | CreateActiveSegment | device,level,reserved[3],version |
      | 11 | AllocateSegmentBlocks | device,level,reserved[3],version,len |
      | 12 | ReleaseSegmentBlocks | device,level,reserved[3],version,range_count,ranges |
      | 13 | RemoveSegment | device,level,reserved[3],version |

  Scenario Outline: large SystemChange Delta item roundtrip
    Given 一个 <change> large SystemChange
    When 编码并解码 Delta item
    Then transaction_tag=1、change_tag=<tag>、flags=0、payload_len 精确且 payload 为 <fields> 并 roundtrip

    Examples:
      | tag | change | fields |
      | 20 | AllocateLargeBlock | device,id,logical_len |
      | 21 | ReleaseLargeBlock | device,id |
      | 22 | RemoveLargeBlock | device,id |

  Scenario Outline: root SystemChange Delta item roundtrip
    Given 一个 <change> root SystemChange
    When 编码并解码 Delta item
    Then transaction_tag=1、change_tag=<tag>、flags=0、payload_len 精确且 payload 为 BlockRef 并 roundtrip

    Examples:
      | tag | change |
      | 30 | AddRoot |
      | 31 | RemoveRoot |

  Scenario Outline: binding SystemChange Delta item roundtrip
    Given 一个 <change> binding SystemChange
    When 编码并解码 Delta item
    Then transaction_tag=1、change_tag=<tag>、flags=0、payload_len 精确且 payload 为 <fields> 并 roundtrip

    Examples:
      | tag | change | fields |
      | 32 | AddBinding | name_len,name,BlockRef |
      | 33 | RemoveBinding | name_len,name |

  Scenario: Application Delta item 使用冻结 envelope
    Given 一个 application delta Bytes
    When 编码并解码 Delta item
    Then transaction_tag=2、change_tag=0、flags=0、payload_len 精确且 roundtrip 恢复相同 Bytes

  Scenario Outline: Delta item 非零 flags 被严格拒绝
    Given 一个 <transaction> Delta item 的 flags 非零
    When decoder 校验 item envelope
    Then 返回 InternalValidationKind::InvalidState 且不发布 State

    Examples:
      | transaction |
      | System |
      | Application |

  Scenario: Delta 保留 System 和 Application 原始顺序
    Given changes 依次为 System A、Application X、System B
    When 编码后再解码 Delta body
    Then changes 顺序仍为 A、X、B

  Scenario: 固定 State 恰好编码十五个 levels
    Given 一个合法 BlockDeviceState
    When 编码并解码基础 State
    Then 连续处理恰好 15 个 level 且不存在 level_count

  Scenario: LevelVersions 窗口尾部必须为 Some Active
    Given 一个 versions 窗口为空或尾部为 None
    When decoder 校验 LevelVersions
    Then 返回 InternalValidationKind::InvalidState

  Scenario: allocation runs 和 released_count 必须一致
    Given released_count 不等于所有 run.len 之和
    When decoder 校验 SegmentData
    Then 返回 InternalValidationKind::InvalidState

  Scenario: release 时序不得早于 allocation 或晚于当前 record
    Given released_at 非零且不满足 allocated_at<released_at<=current record
    When decoder 校验 lifecycle
    Then 返回 InternalValidationKind::InvalidState

  Scenario: Option、runs 和 lifecycle 使用显式编码
    Given BlockDeviceState 含 Some、None、allocation runs、Null release 和 large entries
    When 编码后再解码基础 State
    Then 所有值按第15.3节 roundtrip 且不编码物理物化标志

  @critical
  Scenario Outline: wire 非法输入被严格拒绝
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given wire payload 含有 <invalid>
    When decoder 执行严格校验
    Then 返回 <kind> 且不发布 State

    Examples:
      | invalid | kind |
      | unknown tag | InternalValidationKind::UnknownTag |
      | dynamic-device tag | InternalValidationKind::UnknownTag |
      | invalid Option tag | InternalValidationKind::InvalidState |
      | invalid UTF-8 | InternalValidationKind::InvalidState |
      | invalid State | InternalValidationKind::InvalidState |
      | invalid BlockId | InternalValidationKind::InvalidState |
      | count overflow | InternalValidationKind::InvalidState |
      | length overflow | InternalValidationKind::InvalidState |
      | trailing bytes | InternalValidationKind::TrailingBytes |
       | checkpoint prefix leaves unconsumed bytes | InternalValidationKind::InvalidState |
       | append-log block_seq 超出 RecordSeq 范围 | InternalValidationKind::InvalidState |
      | truncated header | InternalValidationKind::InvalidState |

  Scenario: 重复 registry 项被拒绝
    Given checkpoint registry 含重复 DeviceKey
    When decoder 校验 registry
    Then 返回 InternalValidationKind::InvalidState 且不发布 State

  Scenario: 非升序 map 项被拒绝
    Given checkpoint roots 未按 BlockRef 升序编码
    When decoder 校验 roots
    Then 返回 InternalValidationKind::InvalidState 且不发布 State

  Scenario: System segment payload 的 reserved bytes 必须为零
    Given tag 10、11、12 或 13 的 reserved 三字节非零
    When decoder 校验 payload
    Then 返回 InternalValidationKind::InvalidState 且不发布 State

  Scenario Outline: ReleaseSegmentBlocks 非法 ranges 被拒绝
    Given tag 12 payload 的 ranges 含 <invalid>
    When decoder 校验 ranges
    Then 返回 InternalValidationKind::InvalidState 且不发布 State

    Examples:
      | invalid |
      | 非升序 |
      | 重叠 |
      | len 为零 |
      | start+len 溢出 u32 |

  Scenario Outline: checkpoint 字符串和 application 长度边界
    Given checkpoint 的 <field> 长度为 <length>
    When decoder 在分配前校验长度
    Then 返回 <result>

    Examples:
      | field | length | result |
      | label | 0 | InternalValidationKind::InvalidState |
      | label | 1 | success |
      | label | 255 | success |
      | label | 256 | InternalValidationKind::InvalidState |
      | binding name | 0 | InternalValidationKind::InvalidState |
      | binding name | 1 | success |
      | binding name | 1048576 | success |
      | binding name | 1048577 | InternalValidationKind::InvalidState |
      | application state | 0 | success |
      | application state | 67108864 | success |
      | application state | 67108865 | InternalValidationKind::InvalidState |

  Scenario: checkpoint application remainder 必须完整交给 codec
    Given checkpoint 固定前缀后存在 application payload remainder
    When decoder 解析 checkpoint
    Then remainder 原样交给 codec，codec 未消费或存在非法尾随时返回 InternalValidationKind::InvalidState 且不发布 State

  Scenario Outline: checkpoint 有序集合拒绝重复和乱序
    Given checkpoint 的 <collection> 含 <invalid_order>
    When decoder 校验严格排序
    Then 返回 InternalValidationKind::InvalidState 且不发布 State

    Examples:
      | collection | invalid_order |
      | registry | DeviceKey 乱序 |
      | roots | BlockRef 重复 |
      | bindings | name bytes 乱序 |
      | bindings | name bytes 重复 |
      | large entries | LargeBlockId 乱序 |
      | large entries | LargeBlockId 重复 |

  Scenario Outline: ReleaseSegmentBlocks range 编码边界
    Given tag 12 payload 含 <ranges>
    When 编码并解码 ranges
    Then 结果为 <result>

    Examples:
      | ranges | result |
      | range_count=0 | success |
      | start=0,len=1 | success |
      | start=0,len=1;start=1,len=1 | success |
      | start+len 溢出 u32 | InternalValidationKind::InvalidState |
      | range_count=1000001 | InternalValidationKind::InvalidState |
      | range_count 与 payload_len 不一致 | InternalValidationKind::InvalidState |

  Scenario Outline: application Delta 长度边界
    Given Application Delta payload 长度为 <length>
    When encoder 或 decoder 在分配前校验长度
    Then 返回 <result>

    Examples:
      | length | result |
      | 0 | success |
      | 16777216 | success |
      | 16777217 | InternalValidationKind::InvalidState |

  Scenario Outline: 所有 collection count 在分配前校验上限
    Given <collection> 的 count 为 <count>
    When encoder 或 decoder 在分配前校验 count
    Then 返回 <result>

    Examples:
      | collection | count | result |
      | registry | 1000000 | success |
      | roots | 1000000 | success |
      | roots | 1000001 | InternalValidationKind::InvalidState |
      | bindings | 1000000 | success |
      | bindings | 1000001 | InternalValidationKind::InvalidState |
      | versions | 1000000 | success |
      | versions | 1000001 | InternalValidationKind::InvalidState |
      | allocation runs | 1000000 | success |
      | allocation runs | 1000001 | InternalValidationKind::InvalidState |
      | released entries | 1000000 | success |
      | released entries | 1000001 | InternalValidationKind::InvalidState |
      | large entries | 1000000 | success |
      | large entries | 1000001 | InternalValidationKind::InvalidState |
      | Delta changes | 1000000 | success |
      | Delta changes | 1000001 | InternalValidationKind::InvalidState |
      | release ranges | 1000000 | success |
      | release ranges | 1000001 | InternalValidationKind::InvalidState |

  Scenario: 整条 body 超过 u32 envelope 上限时编码失败
    Given 各局部字段合法但 checkpoint 或 Delta body 总长超过 u32::MAX
    When encoder checked 计算 append-log payload length
    Then 返回 RecordTooLarge 且不调用 append log

  Scenario Outline: State ID 与 lifecycle 数值边界
    Given wire State 包含 <field>=<value>
    When decoder 校验基础 State
    Then 返回 <result>

    Examples:
      | field | value | result |
      | SegmentVersion | 0 | InternalValidationKind::InvalidState |
      | SegmentVersion | 0x0fff_ffff | success |
      | LargeBlockId | 0 | InternalValidationKind::InvalidState |
      | LargeBlockId | 2^60-2 | success |
      | next_large_id | 2^60-1 | success |
      | allocated_at | 0 | InternalValidationKind::InvalidState |
      | released_at | 0 | success |
      | released_at | current_record | success |

  Scenario Outline: allocation runs 必须使用严格递增 sequence 和非零 len
    Given SegmentData 的 allocation runs 为 <runs>
    When decoder 校验 runs
    Then 返回 <result>

    Examples:
      | runs | result |
      | allocated_at=1,len=1;allocated_at=2,len=1 | success |
      | allocated_at=1,len=1;allocated_at=1,len=1 | InternalValidationKind::InvalidState |
      | allocated_at=2,len=1;allocated_at=1,len=1 | InternalValidationKind::InvalidState |
      | allocated_at=1,len=0 | InternalValidationKind::InvalidState |

  Scenario Outline: LevelVersions 窗口结构必须合法
    Given 一个 level 的 versions 为 <window>
    When decoder 校验窗口结构
    Then 返回 <result>

    Examples:
      | window | result |
      | 单个 Some 空 Active | success |
      | 空窗口 | InternalValidationKind::InvalidState |
      | 尾项 None | InternalValidationKind::InvalidState |
      | 非尾部 Some 且 allocation_runs 为空 | InternalValidationKind::InvalidState |

  Scenario: unknown record_kind 经 open 映射 Decode
    Given durable record 的 record_kind 为 0、3 或 255
    When open 严格解码该 record
    Then 返回 OpenError::RecoveryFailed、kind=Decode 且 source.kind=InternalValidationKind::UnknownTag

  @critical
  Scenario: 独立 fixture generator 不调用生产 codec
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 独立 golden fixture generator 和生产 codec
    When generator 生成 header、State、Delta、BlockId 和 BlockRef fixtures
    Then generator 不复用生产 codec 且双方输出逐字节一致
