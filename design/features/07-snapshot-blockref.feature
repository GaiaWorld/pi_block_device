# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: SS-01, SS-02, SS-03, SS-04, SS-05, SS-06, SS-07, SS-08, SS-09, SS-10, SS-11, SS-12, SS-13, SS-14, SS-15, SS-16, SS-17, SS-18, SS-19, SS-20, SS-21, SS-22, SS-23, SS-24, SS-25, SS-26, SS-27, SS-28, SS-29, SS-30
Feature: snapshot、BlockRef 与读取语义
  # 2026-10-11 最新决定：SB 直接 ArcSwap<State>；Transaction<'a,S,C,D,R> 借用具体 runtime 字段。
  # baseline/candidate 初始同一 Arc，仅 Arc::clone，首次修改 Arc::make_mut；无 begin State clone。
  # 撤销事务整体 baseline_leases 合同；当前不提供事务基准物理保护，事务 read 仍 TODO。
  # snapshot 自身全设备 leases、ReadBuf 独立保护及 file/mmap/reclaim 安全合同不变。
  # open_snapshot/commit 使用 publication_access 协调 lease 获取与发布的接线仍 TODO。
  # 无 phase/closed/WriteGuard，取消/终态协议未提供，旧集成测试保留待迁移。
  # segment_infos/live_blocks 尚未批准公开签名，以下相关场景仅列待定，本轮不新增接口。
  作为读取者
  我希望 BlockRef 在固定 State 中被严格验证
  以便旧 snapshot 和当前 State 的读取结果可观察

  @critical
  Scenario: release 前创建的旧 snapshot 保留旧 State
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given BlockRef B 在当前 State 中 live
    When release B 前创建 snapshot
    Then snapshot 固定包含 B 的读取视图

  Scenario: release 和 remove 后旧 snapshot 仍可读取
    Given 旧 snapshot 已在 release 前固定 B
    When 新事务 release 并 Remove B
    Then 旧 snapshot 读取 B 返回原始完整数据

  Scenario: release 后新 snapshot 拒绝 BlockRef
    Given B 已被新事务 release 并 Remove
    When 创建新 snapshot 并读取 B
    Then 返回 SuperblockError::InvalidBlockRef 且 backend read 未调用

  Scenario: 未注册 DeviceKey 返回 SuperblockError
    Given snapshot 的 registry 不包含 DeviceKey 9
    When 通过 SuperblockSnapshot 读取该 BlockRef
    Then 返回 SuperblockError::InvalidBlockRef 且 backend read 未调用

  Scenario: 非法 BlockId 返回 SuperblockError
    Given BlockRef 携带保留 BlockId u64::MAX
    When 通过 SuperblockSnapshot 读取该 BlockRef
    Then 返回 SuperblockError::InvalidBlockRef 且 backend read 未调用

  Scenario: 不属于固定 State 的 BlockRef 返回 SuperblockError
    Given BlockRef 属于注册设备但不属于 snapshot 固定 State
    When 通过 SuperblockSnapshot 读取该 BlockRef
    Then 返回 SuperblockError::InvalidBlockRef 且 backend read 未调用

  @critical
  Scenario: release 前旧 snapshot 读取 released block
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given snapshot 在 B release 前创建且固定 State 中 B 为 live
    When 后续事务 release B 但物理对象尚未 remove
    Then 旧 snapshot 读取 B 返回完整数据

  Scenario: release 后新 snapshot 拒绝 released BlockRef
    Given B 已在当前 State 中设置 released_at
    When release 后创建新 snapshot 并读取 B
    Then 返回 SuperblockError::InvalidBlockRef 且 backend read 未调用

  Scenario: Superblock read 观察当前 State
    Given B 在当前 State 中 live 且随后可被新 commit remove
    When remove 前调用 Superblock::read(B)
    Then 返回 B 的完整数据且返回的 ReadBuf 在释放前阻止物理删除

  Scenario: Superblock read 返回的 ReadBuf 保护对象
    Given Superblock::read 已返回 ReadBuf
    When retire worker 尝试删除 ReadBuf 对应对象
    Then worker 等待 ReadBuf 释放后才完成删除

  Scenario: devices 按 DeviceKey 升序返回
    Given registry 含 DeviceKey 3、1、2
    When snapshot 调用 devices
    Then 返回顺序为 1、2、3

  Scenario: device_state 对未知 key 返回 None
    Given snapshot registry 不含 DeviceKey 9
    When snapshot 调用 device_state(9)
    Then 返回 None

  Scenario: segment_infos 按 level 和 version 排序
    Given DeviceKey 1 有多个普通 segment
    When snapshot 调用 segment_infos
    Then 结果按 level 升序再按 SegmentVersion 升序返回

  Scenario: 非法 SegmentId 返回 SuperblockError
    Given 查询参数包含不属于固定 State 的 SegmentId
    When snapshot 调用 live_blocks
    Then 返回 SuperblockError::InvalidBlockRef 且 backend read 未调用

  Scenario: live_blocks 按 slot 顺序返回
    Given 一个 segment 含多个 live slot
    When snapshot 调用 live_blocks
    Then BlockRef 按 slot/index 升序返回

  Scenario: 空 roots 和 bindings 返回空迭代器
    Given 固定 State 没有 roots 和 bindings
    When snapshot 调用 roots 和 bindings
    Then 两个查询都返回空迭代器

  Scenario: roots 按 BlockRef 排序并按值返回
    Given roots 含多个 BlockRef 和 published_at
    When snapshot 调用 roots
    Then 按 BlockRef tuple 升序返回 RootVersion 值

  Scenario: get_root 命中时按值返回 published_at
    Given 固定 State 的 roots 保存 B 到 RecordSeq R
    When snapshot 调用 get_root(B)
    Then 返回 Some(RootVersion { block: B, published_at: R }) 且结果为派生 Copy 值

  Scenario: get_root 未命中时返回 None
    Given 固定 State 的 roots 不包含 BlockRef B
    When snapshot 调用 get_root(B)
    Then 返回 None

  Scenario: bindings 按原始名称字节排序
    Given bindings 含多个有效 BindingName
    When snapshot 调用 bindings
    Then 按原始名称 UTF-8 字节升序返回名称和 BlockRef

  Scenario: get_binding 按原始 UTF-8 字节精确命中并按值返回
    Given bindings 含名称字节为 hot 的 BindingName 并映射 BlockRef B
    When snapshot 调用 get_binding(hot)
    Then 返回 Some(B)、BlockRef 按值复制且不借用 map entry

  Scenario: get_binding 不执行大小写折叠
    Given bindings 仅含名称字节为 hot 的 BindingName
    When snapshot 调用 get_binding(Hot)
    Then 返回 None

  Scenario: get_binding 不执行 Unicode 规范化
    Given bindings 仅含原始 UTF-8 字节为预组合 U+00E9 的 BindingName
    When snapshot 使用原始 UTF-8 字节为 U+0065 U+0301 的 BindingName 调用 get_binding
    Then 返回 None 且两个规范等价名称仍按不同原始字节键处理

  Scenario: get_binding 未命中返回 None
    Given bindings 不含名称字节为 cold 的 BindingName
    When snapshot 调用 get_binding(cold)
    Then 返回 None

  @critical
  Scenario: snapshot record_seq 和 application 在新 commit 后保持固定
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given snapshot 创建时固定 record_seq=R 和 application state A
    When Superblock 发布 record_seq=R+1 和 application state B
    Then 旧 snapshot 的 record_seq 仍为 R、application 仍为 A 且新 snapshot 返回 R+1 和 B

  Scenario: open_snapshot 按 DeviceKey 顺序获取全部 lease
    Given registry 含 DeviceKey 3、1、2 且全部 lease 可取得
    When 调用 open_snapshot
    Then acquire_snapshot_lease 调用顺序为 1、2、3 且返回完整 snapshot

  Scenario Outline: open_snapshot 获取失败释放全部既有 lease
    Given DeviceKey 1、2、3 依次获取 lease 且 DeviceKey <failed_key> 获取失败
    When 调用 open_snapshot
    Then 返回 SuperblockError::Device、operation=acquire_snapshot_lease、DeviceKey=<failed_key>、保留 source、此前 lease 各释放一次、后续 key 不请求且不返回部分 snapshot

    Examples:
      | failed_key |
      | 1 |
      | 2 |
      | 3 |

  @critical
  Scenario: snapshot 独立持有 backend 强引用和固定 State lease
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given open_snapshot 已固定 Arc<SuperblockState>、每个设备 Arc<D> 和对应 SnapshotLease
    When 释放原 Superblock 以及调用方持有的外部 device map 后读取固定 State 中 live 的 BlockRef
    Then snapshot 仍通过自身强 Arc<D> 和 lease 完成读取，直到 snapshot 与其 ReadBuf 全部释放前物理回收保持阻塞

  Scenario Outline: Superblock 和 snapshot read 映射 DeviceError
    Given <reader> 读取 DeviceKey <key> 的 live BlockRef 时 backend read 返回 DeviceError source
    When read future 返回
    Then 返回 SuperblockError::Device、operation=read、DeviceKey=<key>、保留 source 且不返回部分 ReadBuf

    Examples:
      | reader | key |
      | Superblock::read | 1 |
      | SuperblockSnapshot::read | 2 |

  @critical
  Scenario: Superblock read 临时 lease 失败保留完整上下文
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given Superblock::read 验证 DeviceKey 3 的 BlockRef live 后 acquire_snapshot_lease 返回 DeviceError source
    When read future 返回
    Then 返回 SuperblockError::Device、operation=acquire_snapshot_lease、DeviceKey=3、保留 source、backend read 调用次数为零且不返回部分 ReadBuf
