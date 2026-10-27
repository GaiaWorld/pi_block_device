# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: BW-01, BW-02, BW-03, BW-04, BW-05, BW-06, BW-07, BW-08, BW-09, BW-10, BW-11, BW-12, BW-13, BW-14, BW-15, BW-16, BW-17, BW-18, BW-19, BW-20, BW-21, BW-22, BW-23, BW-24, BW-25, BW-26, BW-27, BW-28
# 方案 A 声明阶段：受影响运行时为 todo 占位；以下为待实现合同，不是验收通过或目录 fsync 已实现的声明。
# prepare(self) 消费批次；成功时数据完整、最终布局可恢复定位、必要文件 DataAndMetadata sync 真实成功先于 metadata durable；2026-10-08 排除目录项突然掉电未落盘，不表示父目录 fsync。
# Drop 仅释放资源，不删除可能已提交的数据；未提交残留由权威恢复状态驱动 reconcile，Unknown 由 pi_superblock 恢复裁决。
# 旧规则 ID 映射及旧测试待迁移，旧三终态规则已失效。
# 2026-10-11 撤销跨事务身份校验和 phase/closed/WriteGuard 机制；旧关闭/即时取消释放断言不再是当前合同。
# 下述取消/终态旧段已直接同步：只保留数据/错误目标，不承诺当前骨架已有取消围栏或终态保护。
Feature: Bytes 与 stream 精确长度
  作为数据写入者
  我希望每个物理写入严格匹配 logical_len
  以便短写和超长写都在提交前确定失败

  Scenario: logical_len 为零拒绝且事务保持可用
    Given write_stream_on 的 logical_len 为 0
    When 调用写入 API
    Then 返回 StreamWriteError::Superblock(SuperblockError::InvalidBlockRef)、不产生 allocation 或 write 且同一事务可继续使用

  Scenario Outline: BlockWrite 构造校验精确长度
    Given logical_len 为 N 的 AllocatedBlock 和长度为 <length> 的 Bytes
    When 调用 BlockWrite::new
    Then 结果为 <result> 且长度不匹配时 backend 不调用

    Examples:
      | length | result |
      | N-1 | construction failure |
      | N | success |
      | N+1 | construction failure |

  Scenario: BlockWrite 构造成功消费线性凭证
    Given 一个尚未消费的 AllocatedBlock 和精确长度 Bytes
    When BlockWrite::new 构造成功
    Then 凭证归 BlockWrite 所有且调用方不能再次使用该凭证

  @critical
  Scenario Outline: 裸 BlockRef 不授予写权限
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given crate 外部调用方只持有一个裸 BlockRef
    When 编译尝试 <operation>
    Then 编译失败且 AllocatedBlock 保持 opaque、不可 Clone/Copy；不提供跨事务身份归属证明

    Examples:
      | operation |
      | 用 BlockRef 构造 BlockWrite |
      | 绕过 BlockWrite 直接调用 backend write |
      | 覆盖 BlockRef 指向的已提交块 |

  @critical
  Scenario: 512 KiB Bytes 写入成功
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given payload 长度为 512 KiB
    When 调用 write_on
    Then 使用普通 level 14 并返回最终 BlockRef

  Scenario: 512 KiB 加一字节使用独立大块
    Given payload 长度为 512 KiB+1
    When 调用 write_on
    Then 使用 independent large block 且写入精确长度

  Scenario: stream 少一个字节返回长度错误（不承诺终态）
    Given 声明长度为 N 的 stream 仅产生 N-1 字节
    When stream 正常结束
    Then 返回 StreamTooShort、全部批次只释放本地资源，物理残留留待权威恢复状态驱动 reconcile、终态/取消围栏协议未提供，不将释放时机视作已有保证

  Scenario: stream 多一个字节在写前返回长度错误（不承诺终态）
    Given written=N 且下一非空 chunk 长度为 1
    When 检查该 chunk
    Then StreamTooLong 报告 written=N 和 logical_len=N、该 chunk 零字节写入、全部批次只释放本地资源，物理残留留待权威恢复状态驱动 reconcile、终态/取消围栏协议未提供，不将释放时机视作已有保证

  Scenario: 达到 N 后额外非空 chunk 被拒绝
    Given stream 已写入恰好 N 字节
    When EOF 前出现额外非空 chunk
    Then 返回 StreamTooLong 且额外 chunk 不调用 write

  Scenario: 空 chunk 不表示完成
    Given stream 在有效数据间产生空 Bytes chunk
    When 消费该 chunk
    Then written 不增加且仍等待后续 chunk 或 EOF

  Scenario Outline: 多 chunk stream 精确写满或拒绝非法 checked position
    Given logical_len=<logical_len> 且 stream 依次产生长度为 <chunk_lengths> 的 chunk，连续 offset 先经 BlockWritePosition::new(offset,logical_len) 校验
    When 每个非空 chunk 仅调用三参数 BlockWriteChunk::new(block,chunk,position)，并在 <termination> 后观察结果
    Then <result>

    Examples:
      | logical_len | chunk_lengths | termination | result |
      | N | 1, N-1 | EOF | 每个 request 的 offset 和 logical_len getter 保持位置值，仅最后一个 request 的 is_last 为 true，write 逐次消费单一 request，transaction 保持可提交并返回最终 BlockRef |
      | N | 0, 1, N-1 | EOF | 空 chunk 不构造 request、不增加 written，后续合法 chunk 仍按连续 offset 写入并成功 |
      | 0 | 1 | position construction | BlockWritePosition 返回 InvalidBlockWritePosition，allocation、batch、backend 和 write 均不调用 |
      | N | N+1 | chunk construction | BlockWriteChunk 返回 InvalidBlockWriteChunk，超界 chunk 不调用 write，终态协议未提供，不删除可能已提交的物理数据 |

  Scenario: stream item error 返回 item 错误（不承诺终态）
    Given write_stream_on 已写入部分 chunk 且下一项返回 Item(E)
    When 消费该 stream item
    Then 返回携带 E 的 stream item error、全部批次只释放本地资源，物理残留留待权威恢复状态驱动 reconcile、终态/取消围栏协议未提供，不将释放时机视作已有保证

  @critical
  Scenario: Stream next await 取消释放资源而不删除数据
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given write_stream_on 正在等待 Stream::next
    When 调用方取消 future
    Then 不删除可能已提交的数据，残留留待权威恢复状态驱动 reconcile；取消时批次和 writer guard 的立即释放及终态围栏协议未提供

  Scenario: backend write await 取消释放资源而不删除数据
    Given 写入 API 正在等待 backend write
    When 调用方取消 future
    Then 不删除可能已提交的数据，残留留待权威恢复状态驱动 reconcile；取消时批次和 writer guard 的立即释放及终态围栏协议未提供

  Scenario: commit prepare await 取消释放资源而不删除数据
    Given commit 正在等待某个 DeviceKey 的 prepare
    When 调用方取消 future
    Then 不删除可能已提交的数据，残留留待权威恢复状态驱动 reconcile；取消时批次和 writer guard 的立即释放及终态围栏协议未提供

  Scenario: 取消不触发物理删除
    Given write 或 prepare future 被取消
    When 通过字段自然 RAII 释放本地资源
    Then 不删除可能已提交的数据，未提交残留由基于权威恢复状态的 reconcile 回收，Unknown 由 pi_superblock 恢复裁决

  Scenario: Application codec 不接受 stream 输入
    Given 一个 ApplicationDelta 需要同步编码
    When 外部调用方尝试以 Stream 作为 codec 输入
    Then 编译失败且 application codec 不接受 Stream

  @critical
  Scenario Outline: alloc_on 为十五个普通 level 选择最小可容纳级别
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given DeviceKey 1 已注册且请求长度为 <length>
    When transaction 调用 alloc_on(1,length)
    Then 返回使用普通 <level> 的线性 AllocatedBlock

    Examples:
      | length | level |
      | 1 | level 0 |
      | 4096 | level 0 |
      | 4097 | level 1 |
      | 6144 | level 1 |
      | 6145 | level 2 |
      | 8192 | level 2 |
      | 8193 | level 3 |
      | 12288 | level 3 |
      | 12289 | level 4 |
      | 16384 | level 4 |
      | 16385 | level 5 |
      | 24576 | level 5 |
      | 24577 | level 6 |
      | 32768 | level 6 |
      | 32769 | level 7 |
      | 49152 | level 7 |
      | 49153 | level 8 |
      | 65536 | level 8 |
      | 65537 | level 9 |
      | 98304 | level 9 |
      | 98305 | level 10 |
      | 131072 | level 10 |
      | 131073 | level 11 |
      | 196608 | level 11 |
      | 196609 | level 12 |
      | 262144 | level 12 |
      | 262145 | level 13 |
      | 393216 | level 13 |
      | 393217 | level 14 |
      | 524288 | level 14 |

  Scenario: alloc_on 超过 level 14 上界时选择独立大块
    Given DeviceKey 1 已注册且请求长度为 524289
    When transaction 调用 alloc_on(1,524289)
    Then 返回 independent large block 的线性 AllocatedBlock

  Scenario Outline: alloc_on 拒绝非法目标或耗尽
    Given alloc_on 的条件为 <condition>
    When transaction 请求分配
    Then 返回 SuperblockError::InvalidBlockRef、不产生 allocation change、batch 或 backend I/O

    Examples:
      | condition |
      | DeviceKey 未注册 |
      | len=0 |
      | 普通 SegmentVersion 耗尽 |
      | LargeBlockId 达耗尽哨兵 |

  Scenario: write_batch 多请求全部成功
    Given 同一 transaction 的多个合法 BlockWrite 跨 DeviceKey 2、1、2
    When 调用 write_batch
    Then 每个请求恰好完成一次 write、对应 participant 可继续 prepare 且所有凭证标记为完整写入

  Scenario: write_batch 不在本事务分配记录中的 block 预检拒绝
    Given writes 含一个 block 不在本事务 Vec<AllocationRecord> 中的合法 BlockWrite
    When write_batch 执行整批预检
    Then 返回 Result<(),SuperblockError> 的 SuperblockError::InvalidBlockRef、所有 backend begin_write_batch 和 write 调用次数均为零且 transaction 保持可用

  Scenario Outline: BlockWrite 构造期长度不匹配拒绝且不可进入 write_batch
    Given logical_len 为 N 的 AllocatedBlock 和长度为 <length> 的 Bytes
    When 调用 BlockWrite::new
    Then 返回构造错误、无法形成 BlockWrite、backend I/O 和 metadata append 均不调用

    Examples:
      | length |
      | N-1 |
      | N+1 |

  @critical
  Scenario: write_batch 重复同一 AllocatedBlock 在类型系统中不可达
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 调用方已把一个 AllocatedBlock 移入首个 BlockWrite
    When 编译尝试把同一 AllocatedBlock 再次移入第二个 BlockWrite 并组成 write_batch
    Then 编译失败且运行时整批预检不存在重复消费同一 allocation 的输入

  @critical
  Scenario: write_batch 中途 DeviceError 保留错误上下文（不承诺终态）
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given write_batch 已完成一个或多个请求且下一次 write 返回 DeviceError
    When write_batch 返回
    Then 返回 SuperblockError::Device、operation=write、对应 DeviceKey 和 source、全部批次只释放本地资源，物理残留留待权威恢复状态驱动 reconcile、终态及取消协议未提供 且不返回部分成功结果

  Scenario Outline: commit 在 append 前拒绝未完整写凭证
    Given transaction 包含 <state> 的 AllocatedBlock
    When 调用 commit
    Then 先分配下一 RecordSeq但在 physical-written 和凭证纯预检失败时不发布或持久化该序号、返回 CommitFailed、published State 不变、全部批次只释放本地资源，物理残留留待权威恢复状态驱动 reconcile、终态检查及取消协议未提供、下一事务可取得 gate 并可重新使用同一 candidate 序号

    Examples:
      | state |
      | 从未写入 |
      | stream 或 chunk 仅写入 logical_len 的一部分 |

  @critical
  Scenario: key1 batch 已存在时等待 key2 首次 write 取消的外层清理目标
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given transaction 已取得 writer gate、DeviceKey 1 的 begin_write_batch 已返回 batch，DeviceKey 2 同步 begin 成功且正在等待首次 write 的 gate
    When 调用方取消等待 key2 首次 write 的外层写入 future
    Then key1 batch 释放本地资源且不删除可能已提交的数据、key2 不执行任何事务终态且其局部 RAII 资源全部释放、未尝试后续 DeviceKey、终态及取消协议未提供、candidate 和 published State 不变、RecordSeq 未分配或消费、metadata append、publish 和 enqueue_retirements 均未调用且下一 transaction 可取得 gate

  @critical
  Scenario: key1 batch 已存在时 key2 begin_write_batch DeviceError 返回外层错误（不承诺终态）
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given transaction 已取得 writer gate、DeviceKey 1 的 begin_write_batch 已返回 batch 且 DeviceKey 2 的 begin_write_batch 返回 DeviceError E
    When 外层写入 API 处理该错误
    Then 返回 SuperblockError::Device、operation=begin_write_batch、DeviceKey=2、source=E，key1 batch 释放本地资源且不删除可能已提交的数据、key2 不产生可由外层持有的 batch 且局部 RAII 资源全部释放、未尝试后续 DeviceKey、终态及取消协议未提供、candidate 和 published State 不变、RecordSeq、metadata append、publish 和 retirement 登记均无副作用且不返回部分成功结果
