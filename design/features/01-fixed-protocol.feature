# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: FP-01, FP-02, FP-03, FP-04, FP-05, FP-06, FP-07, FP-08, FP-09, FP-10, FP-11, FP-12
Feature: 固定块协议
  作为协议使用者
  我希望块几何、BlockId、BlockRef 和 RecordSeq 保持冻结
  以便三个 crate 共享同一持久化合同

  @critical
  Scenario: 普通 BlockId 保持既有位布局
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 一个 level 为 2、segment_version 为 0x0123456、block_index 为 0x89abcdef 的普通 BlockId
    When 将 BlockId 编码为 u64 再解码
    Then level、segment_version 和 block_index 恢复为原值

  Scenario: 普通 BlockId 拒绝零 SegmentVersion
    Given 一个普通 BlockId 的 SegmentVersion 为 0
    When 调用协议校验
    Then 校验返回 InvalidBlockId

  Scenario: 普通 BlockId 接受最大合法字段
    Given 一个普通 BlockId 的 level 为 14、SegmentVersion 为 0x0fff_ffff 且 block_index 为 u32::MAX
    When 编码后再解码该 BlockId
    Then 三个字段均恢复为输入的最大合法值

  Scenario: 全一位 BlockId 保留值被拒绝
    Given 一个编码值为 u64::MAX 的 BlockId
    When 调用协议校验
    Then 校验返回 InvalidBlockId

  @critical
  Scenario: 固定几何完整匹配十五项 KiB 表
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 一个 backend 声明固定块协议能力
    When 查询 base size、普通等级表和 independent large block 能力
    Then base size 为 4096 字节且表为 [4096, 6144, 8192, 12288, 16384, 24576, 32768, 49152, 65536, 98304, 131072, 196608, 262144, 393216, 524288] 并且 independent large block 为 true

  Scenario: level 15 使用独立大块 ID
    Given 一个 large_block_id 为 42 的独立大块
    When 编码并解码其 BlockId
    Then level 为 15 且 high 60 bits 恢复为 42

  Scenario: large block 拒绝零 ID
    Given 一个 level 15 BlockId 的 large_block_id 为 0
    When 调用协议校验
    Then 校验返回 InvalidBlockId

  Scenario: large block 接受最大有效 ID
    Given 一个 level 15 BlockId 的 large_block_id 为 2^60-2
    When 编码后再解码该 BlockId
    Then large_block_id 恢复为 2^60-2

  Scenario: large block 耗尽哨兵不能分配
    Given LargeBlocks.next_id 为 2^60-1
    When 请求分配新的 independent large block
    Then 分配被拒绝且 next_id 不回绕

  @critical
  Scenario: BlockRef 使用十二字节小端编码
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given DeviceKey 为 7 且 BlockId 为有效值
    When 编码 BlockRef
    Then 输出依次为四字节 DeviceKey、八字节 BlockId 的 little-endian 字节且长度为 12

  Scenario: RecordSeq 有效范围排除 Null 和耗尽哨兵
    Given RecordSeq 使用 u32 表示
    When 校验 0、1、u32::MAX-1 和 u32::MAX
    Then 0 是 Null、1..=u32::MAX-1 是有效记录且 u32::MAX 是耗尽哨兵

  Scenario: 生命周期记录不允许回绕
    Given 当前 RecordSeq 为 u32::MAX-1
    When 请求生成下一有效 RecordSeq
    Then 返回 RecordSeqExhausted 且不生成 0
