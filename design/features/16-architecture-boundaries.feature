# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: AB-01, AB-17, AB-02, AB-03, AB-04, AB-05, AB-06, AB-07, AB-08, AB-09, AB-10, AB-11, AB-12, AB-13, AB-14, AB-15, AB-16
Feature: 三 crate 架构边界
  # 2026-10-08 用户修改故障模型：排除目录项突然掉电未落盘，不把该范围描述为原合同。
  # prepare 的必要文件 DataAndMetadata sync 真实成功先于 metadata；reconcile synced=true 不表示父目录 fsync。
  # 创建发布删除错误、原子 noreplace、取消及进程退出恢复仍考虑；不新增 runtime 或 std 文件 I/O 旁路。
  # 声明阶段批准：pi_block_device 与 pi_superblock 精确依赖 rpds 1.1.0。
  # roots/bindings/LargeBlocks.entries 使用 RedBlackTreeMapSync；版本与段数据使用 VectorSync。
  # Sync 别名实际采用 ArcTK/triomphe::Arc；十五级数组保留，非默认 Rc。
  # DeviceTable 静态 Arc<[DeviceRegistration]> 与动态 VectorSync<BlockDeviceState> 等长，按一基 DeviceKey 对应。
  # DeviceSlot 保留构造输入；get/iter 提供 DeviceSlotRef 借用视图，不 clone 整 slots 或 Arc::make_mut。
  # ApplicationState::clone 的廉价性由应用保证。集合结构共享不替代 lease、发布锁、writer gate 或 backend 并发保护。
  # 相关运行时方法仅有 todo；旧测试保留待迁移，不代表实现验收通过。
  # 当前差异而非合同修订：WriteBatch 只有 write/prepare，backend 无多段/stream 入口；事务层 write_stream_on 仍引用 futures_core::Stream 且为 todo。
  # futures-core 当前在三个 Cargo 清单均声明；两个底层 crate 的 src 无直接引用，但旧 tests 有引用，不可称整个 crate 确实未用。AB-08 保留，后续依赖归属/测试迁移待裁决。
  # futures-lite 当前为文件后端生产依赖，用于目录流，与下列开发依赖限定仍有差距；不修改冻结规则迎合 Cargo。
  # 用户批准本轮 Active/Sealed/large 完成不可变范围 mmap，普通段单文件加固定 slot offset；无 owned fallback。
  # AB-17 上层多段/流写仍待实现；0.1.0 mmap 强 flush 冲突属于历史，当前固定 0.1.2 已实测映射存活时真实强 flush 成功，不跳过 flush。
  # 已批准 retirement 边界：enqueue 仅登记去重，外部定时 await reclaim_retirements 的有限轮；内部异步串行化，不暴露 token，无 sleep/timer/spawn/空队列等待，不决定锁算法。
  # 文件后端有限 reclaim、write/prepare 与当前模型文件 reconcile 已实现，旧线程/sleep/block_on 移除；取消保留围栏/责任。目录 fsync 不存在，root 独占与端到端 commit/recovery 未完成。
  作为库维护者
  我希望依赖、公开 API 和运行时边界不越权
  以便协议层与具体后端可以独立演进

  @critical
  Scenario: pi_superblock 只依赖协议层和 append log
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given workspace 包含三个 publishable crate
    When 检查 pi_superblock 的 Cargo 依赖
    Then pi_superblock 依赖 pi_block_device 和 pi_append_log且绝不依赖 pi_file_block_device

  @critical
  Scenario: pi_superblock 保留 metadata 多段和流写目标
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given pi_superblock 通过内部 SuperblockAppendLog 持有用户授权 D:/dev/pi_append_log 的精确 0.4.0 本地源码，pi_async_fs 解析为 0.1.2
    When 执行 metadata 多段写或异步流写
    Then 应使用 pi_append_log 的多段写和异步流写能力并通过 pi-async-fs 完成异步文件 I/O，不退回逐段同步写；该目标待实现，当前 adapter 使用单一 encoded buffer，库编译兼容不是该目标验收

  Scenario: pi_file_block_device 依赖 pi_block_device
    Given workspace 的文件后端 crate
    When 检查其依赖图
    Then pi_file_block_device 依赖 pi_block_device 且不承担 superblock 日志职责

  Scenario: pi_block_device 不包含具体文件实现
    Given 外部调用方检查 pi_block_device 公开项
    When 查找 filesystem backend、Superblock 和 allocation log
    Then 这些具体实现不存在

  @critical
  Scenario: Superblock 不暴露 public log trait 或日志泛型
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 外部调用方使用 Superblock<S,C,D,R>
    When 检查其公开类型参数和日志 API
    Then Superblock 没有 log 泛型且不存在 public log trait

  Scenario: 公开 API 不提供动态设备 change
    Given 一个已打开的 Superblock
    When 外部调用方查找新增、替换、重命名和注销设备 API
    Then 这些 API 不存在

  Scenario: 生产 API 不引用 Tokio 类型
    Given 三个 crate 在生产依赖中不启用 Tokio
    When 编译公开异步签名
    Then 公开签名不引用 Tokio 类型且保持 runtime-neutral

  Scenario: 生产 API 不依赖 async-trait 或 serde
    Given 三个 crate 的生产依赖清单
    When 检查 async-trait 和 serde
    Then 两者均不在生产依赖中

  Scenario: futures-core 只由 pi_superblock 使用
    Given 三个 crate 的依赖清单
    When 检查 futures-core 的声明位置
    Then 只有 pi_superblock 声明 futures-core

  Scenario: futures-lite 只在批准范围使用
    Given 三个 crate 的依赖清单
    When 检查 futures-lite 的声明位置
    Then pi_superblock 可声明生产依赖且 pi_file_block_device 仅可声明开发依赖

  Scenario: Superblock 使用批准的精确 SmallVec 依赖
    Given workspace 的生产依赖清单
    When 检查 pi_superblock 的 smallvec 和 arc-swap
    Then 精确依赖 smallvec=1.15.2 和 arc-swap=1.9.2；SB devices 使用私有内联容量4的 SmallVec，容量不是设备限制且不增加公开const泛型，published为直接ArcSwap而非OuterArc

  Scenario: 依赖版本和 Rust 版本固定
    Given workspace 使用批准依赖版本
    When 使用 nightly-2026-08-17-i686-pc-windows-gnu 执行 cargo check --locked
    Then 检查成功且依赖版本未被锁文件替换

  Scenario: 文件后端使用 pi-async-fs 且不使用 memmap2
    Given pi_file_block_device 需要异步文件读写、lease 保护和多段写
    When 检查其生产依赖和文件 I/O 实现边界
    Then 使用 pi-async-fs 完成异步文件操作、多段写和异步流写，不直接声明或使用 memmap2；Active、Sealed 和 large 的已完成不可变范围均使用 pi-async-fs 不透明只读 mmap 句柄，普通段按固定 slot offset 映射且 append 后仅建立新不相交范围

  Scenario: Application codec 不越过 superblock 边界
    Given ApplicationState 和 ApplicationDelta 通过 pi_superblock codec 编码
    When 检查 pi_block_device 和 pi_file_block_device 的公开项
    Then 两个底层 crate 不暴露 application log 或 superblock State

  Scenario: enum adapter 统一异构 backend
    Given 应用定义包含 file、memory 和 remote 变体的 AppDevice
    When 在 AppDevice 上实现 BlockDeviceBackend
    Then pi_superblock 只接收统一 backend 类型 D

  Scenario: 无旧 aggregate facade
    Given workspace 发布三个新 crate
    When 外部调用方查找旧单体 aggregate facade
    Then 旧 facade 不作为公开发布边界存在

  @critical
  Scenario: 单一内部 pi_append_log 是完整逻辑 State 唯一持久权威
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 超级块日志、backend manifest、介质 metadata 和 retirement queue 同时存在
    When open 或 reconcile 裁决 allocation、roots、bindings、application 和 retirement responsibility
    Then 仅内部 SuperblockAppendLog 恢复的完整逻辑 State 具有权威性，manifest、介质 metadata 和 retirement queue 只能被校验、修复或从该 State 重新推导且不得以冲突内容覆盖 State
    And 回收不改变 durable 事务终态，内存 retirement queue 不是权威持久状态
