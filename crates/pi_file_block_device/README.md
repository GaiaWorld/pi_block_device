# pi_file_block_device

当前实现 read/mmap、有限异步 reclaim、WriteBatch 严格追加/完整性检查/真实文件 sync 后成功，以及权威 State 驱动的文件恢复；不代表 Superblock 端到端运行时完成。

2026-10-08 用户明确修改原故障模型：不考虑突然掉电导致新文件名、rename/remove 目录项未落盘。仍要求数据完整、必要 DataAndMetadata sync 真实成功先于 metadata，并考虑创建发布删除错误、原子 noreplace、取消和进程退出恢复。media_metadata_synced=true 表示此模型所需操作和实际文件 sync 完成，不表示父目录 fsync。没有新增 runtime、std/syscall 生产 I/O 旁路或外部依赖修改。

## 安全入口

用户批准现有 create/open 改为 unsafe，参数不变。Safety 要求从构造 future 首次轮询起，直到设备、在途操作和最后一个映射 ReadBuf 释放，所有不受依赖注册表协调的参与者不截断、覆盖、替换、重建、删除、建立相交映射或破坏存储稳定性。题设没有外部破坏者；此义务仍由公开 unsafe 入口表达，不能由私有 unsafe 隐藏。

生产文件 I/O 全部经 registry 精确依赖 pi_async_fs =0.1.2 的共享进程注册表，不引入 owner/authority 多模式。async-lock =3.4.2 保持不变。superblock 同样精确依赖 =0.1.2，其日志依赖为用户授权的 D:/dev/pi_append_log 本地源码 =0.4.0，不是 registry 发布声明；日志自身的 pi_async_fs="0.1" 解析到同一 0.1.2。

## 读取布局

普通对象为 segment-{level:02x}-{version:08x}.bin，每个 slot 对应固定 level_size，读取 offset 为 slot * level_size。large 为 large-{id:015x}.bin，长度取自 lease.logical_len。虚拟空 Active 无须物理文件；已完成 slot 的 Active、Sealed 和 large 都使用真实不透明只读 mmap，无 owned fallback。

映射范围固定为 slot 半开范围，不重映射整个增长段；append 完成后的新 slot 建立新不相交映射。异步锁串行化映射建立，共享相同 slot 的映射；缓存持有 Weak owner 和独立注销完成元数据，不强持有映射。弱升级失败并不代表底层范围已注销：read 异步等待最后 owner 的 Drop 完整释放句柄后发布完成信号，再建立新映射，不重试 Conflict。Drop 不取得缓存异步锁；短标准锁只保护完成状态和 waker，不跨 await，解锁后唤醒。ReadBuf 同时保留 snapshot lease，不公开 raw mapping。large 超过 1 MiB 可读，物理长度不符以及 usize/isize 地址空间超限返回结构化 Read 错误。

锁定 0.1.2 已真实验证只读映射存活时 Data 与 DataAndMetadata 强 flush 成功。测试先 await append 完成，再 await DataAndMetadata；旧视图不变，新 slot 的不相交映射可读。两个映射同时存活时两种 flush 均成功，并继续 append/两种 flush/读取第三个范围，三个视图均正确。large 映射的 lease 生命周期和最后释放后刷新断言保留。旧 0.1.0 同一迁移测试真实 RED：5 passed/2 Conflict failed；升级后 7 mmap 全 GREEN，没有跳过刷新或修改 registry。

0.1.2 FileIo::flush 合同仅覆盖调用前已成功完成的受管普通写入和长度变更，不保证另一资源仍在进行或随后开始的 append，也不是跨文件全局屏障。取消 append 后必须确认后台完成，再取得新的显式刷新证据。Data 包括恢复内容所必需状态；DataAndMetadata 另含文件自身元信息，均不承诺父目录项、namespace 提交或映射脏页刷新。

## 单轮回收

同步 enqueue 仅登记去重，无任务启动。外部 await reclaim_retirements；内部异步执行锁串行化重叠调用，在取得资格时快照对象身份，后入队留下一轮。每个选定对象最多尝试一次，Guarded/Retry 不妨碍后续对象，报告 attempted/completed/guarded/retryable。无生产线程、sleep、timer 或 block_on。

P2 修复：取得 reclaim_gate 后先冻结 selected 对象身份；空范围立即返回全零，不等待活跃 batch 的 write_gate。只有非空范围才等待 write_gate，等待期间入队对象不扩展本轮。取消等待释放 round gate，尚未执行删除的责任留队；锁与公开协议未扩展。

快照注册与删除准入使用同一短标准锁，无标准 guard 跨 await。删除准入后拒绝该对象的新 lease；取消保留队列责任及准入记录，ID 不复用。普通 Segment 删除一个段文件；成功时清除对应映射缓存记录。已有 ReadBuf/lease 保护阻止删除，依赖活动句柄冲突保留重试责任。

NotFound 完成保留三条件：NotRemovedByOperation、typed std::io::Error::NotFound、try_exists == Ok(false)。RemovedByOperation 可完成；Unknown/未来证据进入 unresolved 记录，保留责任且后续轮次禁止再次 unlink。本轮具体 Local namespace 的普通删除路径不产生 Unknown，恢复裁决未实现，不能用于可替换路径或远端 namespace。

## 未完成边界

begin_write_batch 同步返回空批次，无文件 I/O、锁等待、事务身份或 token。首次 write 才 await 自设备 write_gate，取得后重检 write fence，持锁直到 prepare 完成或批次 Drop；未写批次不阻塞非空 reclaim。等待锁期间取消未发起 I/O，不设置恢复围栏；已发生 I/O 的未决/不完整数据保护不变。回收/对账按 reclaim_gate -> write_gate 顺序取得资格，写入不反向取得 reclaim_gate，映射使用独立准入锁。不同对象允许交错，无全局排序要求；同一段按真实 byte_len 严格匹配 slot * level_size + chunk offset，拒绝缺失前缀、旧 slot 重写、重复、overlap、gap 或变化的 logical_len。虚拟空段首次 slot 0 写入才 create_new；普通逻辑 payload 完整后仅在该 slot 尾部补零，large 单独对象且不补零或复用 ID。append 后再核对真实尾长。

write/prepare 在可能变更的 await 前设置内部未决标记，只有观测到完成才清除。失败或取消禁止批次盲目续写；Drop 将未决 I/O 或不完整块升级为该 device instance 的恢复围栏。只释放句柄/gate，不启动异步 I/O、不 unlink；原始 pi_result Report、CreateFailure 或 BufferFailure（含进度证据）保存在内部结构化错误 wrapper 中。恢复须重新 open 并按权威 State 对账，不能仅重新 begin。required Active 超出权威 slot 边界的完整或不完整未提交尾部可安全截断；不足前缀、Sealed 或 large 长度不符仍报 RequiredObjectCorrupt，不填补或重建。

prepare 消费批次，先检查所有逻辑数据与补零完成，再在实际完成 append 的各句柄上 await DataAndMetadata flush；全部成功才返回 Ok，不遗留实例 write fence。incomplete 优先返回 InvalidData，错误/取消仍围栏且保留残留。文件 sync 不构成目录项抗突然掉电证明；此保证已被用户排除，不是依赖提供了目录同步。Superblock metadata ordering 尚未端到端验证。

reconcile 仅用于 open 恢复，按 reclaim_gate -> write_gate 串行化；短准入锁设置恢复标记，期间拒绝新 lease。先验证全部 required 文件，Active 尾部修复拒绝现有保护 lease，依赖排他 truncate 还拒绝活动/待建立 mapping。截断后真实 sync 才计 completed；孤儿仅按明确删除证据完成，Unknown 保留未决责任不盲删。最后全部 required 文件及 manifest 均实际 DataAndMetadata sync 后，成功报告 completed=planned、remaining=0、partial_progress=false、synced=true。错误或取消保留写围栏，显式重开重新规划；不撤销已完成修复。root 独占、Superblock 权威日志恢复集成仍未完成。

create 保留 staging 的 create_new、完整 append、DataAndMetadata flush、释放句柄、no-replace rename 原子发布。取消前发布 staging 不被 open 接受；最终 manifest/staging 碰撞、完整创建/改名证据和非覆盖行为保持测试。create 仍不承诺目录项抗掉电持久化。

P1 修复：权威 State 当前虚拟空 Active 的零长/半首 slot 残留清理不是退休身份。只在本轮新增删除准入、await 删除明确完成且底层资源/租约已收尾后释放该准入，允许同 version 的 slot 0 首次物化。既有退休或取消准入永不清除，其他孤儿/真正退休 segment 仍永久拒 lease/write；Unknown/未确认错误不释放，取消保持 write fence，显式重复 reconcile 也不自解封既有围栏。

## 验证范围

2026-10-10 同步 begin/首次 write 延迟 gate：原 66 项保留，锁等待回归改为首次 write，而非弱化串行保护；新增 3 项后 file all-targets --locked --offline 为 69 passed。旧实现的未写批次占锁回归成功编译后真实 RED，之后 GREEN；同步签名迁移不记为 RED。block 25、superblock lib 10 通过；file/block Clippy -D warnings、fmt check 通过。Superblock publication 所有权确认前骨架未迁移，实际仍 30 todo!，workspace lib/test-support 的既有 44/45 警告未压制。

本轮 file_child_kill_test.rs 为独立 file-only kill fixture 验收：父进程保留 tempdir、expected device ID 和独立构造的 State oracle，current_exe --exact 子测试仅由 Command.env 传入配置，不修改进程全局环境。精确 stdout ACK 并 flush 后阻塞 stdin，父进程保留 writer；实际 Child.kill + wait 确认非正常成功退出且 Drop witness 未执行。stdout 独立线程持续排空、stderr 继承，60 秒 ACK 超时仅防挂死，不用于判断 I/O 时机；ChildGuard 在失败时 kill/wait 清理。

场景 A ACK 表示 slot0 prepare 实际成功、slot1 两字节 write 实际 await 成功但未 prepare/flush，活跃 batch/device 未 Drop 即 kill。父 oracle 仅有 slot0，确认原文件精确前缀加两字节尾部；reconcile planned=completed=1、remaining=0、synced=true，真实截断/sync 后尾长精确、mmap payload/padding 精确，repeat planned=0，再写 slot1/prepare/read 且旧映射前缀不变。场景 B ACK 表示 ordinary 与 large 1048593 字节全部 write await 和 prepare sync 成功，device、输入 buffers 与新 batch 保持存活时 kill；same-ID open/reconcile planned=0 后精确 mmap 全部 payload，普通 slot padding 全零，large 无额外 padding。

本轮 pinned file --all-targets --locked --offline 实测 65 passed / 0 failed / 0 ignored：原 62 项不改，加 2 个真实 kill 场景和 1 个普通运行时无 child 环境的子测试入口。首次即全部 green，诚实记录为新增验收，不制造 production RED。file all-targets Clippy -D warnings、fmt check、workspace lib check 通过，保留既有 43 个 Superblock 警告。下文 34/48/51/57/62 是历史轮次结果；历史“没有强制终止子进程测试”仅描述该轮。

上述 kill 轮次不是掉电测试，不验证 native append/mapping/remove pending cancel，也不是 Superblock metadata 排序、commit/log/checkpoint 恢复端到端验收。Superblock 当前 32 个 todo! 明确阻塞；该历史轮次未添加 blocking 依赖。

2026-10-09 用户批准仅 file dev-dependencies 增加 blocking =1.7.0（已有缓存/锁版本），本轮 pinned file --all-targets --locked --offline 为 66 passed / 0 failed / 0 ignored：旧 65 项断言全部保留，新增 flush_cancel_tests 的一项父 harness 验收，真实操作只在 fresh current_exe --exact 子进程运行。Command.env 设置 BLOCKING_MAX_THREADS=1，不修改全局环境；stdout 线程持续排空，60 秒 channel timeout 仅防挂死，失败 kill/wait 清理，无 sleep loops。专项和全套首次 green，不制造 RED。file Clippy -D warnings、workspace fmt check、workspace lib check 均通过，保留既有 43 个 Superblock 警告。

公共 begin/write 先 await payload 与 padding 的真实 append 和原句柄 byte_len，预先打开 truncate/overwrite；唯一 blocking worker 的 started channel 确认占用后 poll 消费式 prepare。Pending 不作为准入证据：两个排他 probe 必须 poll_once 立即返回当前 context conflict（pi_result 是传递依赖，使用其稳定 Display），overwrite 还断言 Exact bytes=0、Unchanged 和原 buffer。取消 future 实际 Drop batch 后同实例 begin 拒绝，worker 仍未放行时两 probe 仍 Conflict、原字节不变。生产 flush 路径无改动、无 mocknativeio。

仅 cfg(test) 的 Drop 交接钩子在真实 write fence 设置后接收原 append LocalFile，用其 byte_len 等待 detached flush actor 持有的同一 file-slot lock 归还；worker completion 本身不是 flush/FIFO 屏障。该钩子延长测试句柄寿命，未验证无句柄交接时的 LocalFile 注销路径。完成后真实 truncate(4096) 等长无修改成功，明确观察底层排他 lease 恢复；原取消 actor 的返回结果不再可取得，不宣称其 sync 返回 Ok。随后显式 DataAndMetadata flush 与 same-ID unsafe reopen/reconcile 成功，独立 fixture State 的 planned/completed/remaining=0、synced=true，原始读取和 mmap 的 6 字节 payload 加 4090 零 padding 精确。原实例仍 fenced，重开实例 begin 成功。

本轮取消阶段定义为 actor 已持 flush lease、原资源已交后台且 native 同步未完成；worker 被占用使 native 工作等待执行，不证明取消时 syscall inside，也不靠时长推断精确 enqueue 时点。append、map-create、remove pending cancel 仍未受该 flush 测试覆盖；mappingcancel 无实际泄漏证据，不称修复泄漏。全链 Superblock commit/log/checkpoint/metadata ordering 未验收，32 todo! 未实现。

pinned nightly-2026-08-17-i686-pc-windows-gnu 在升级后的 file --all-targets --locked --offline 实测 34 passed：14 lib、8 manifest/public bounds、7 mmap、2 public reclaim、3 reconcile；零失败/忽略。create/reclaim/mapping-last-drop 既有回归保留。read 与 reclaim 各有成功编译后的真实 RED，再 GREEN。取消测试暂停在删除准入后、底层删除提交前；尚无底层删除提交后的取消动态证据。workspace lib check 通过，保留既有警告。

独立审查修复后本套为 34 passed（原 33 项加 last-drop 回归）。真实 mapping 最后析构通过独立 drop 线程 channel 暂停，public read 原先返回 Conflict（RED）；修复后保持 Pending，取消等待者再读、完成真实注销后成功（GREEN），无 sleep 或无限冲突重试。补回 required-object source.is_some 与 temporary planned=1、remaining=0 断言。此证据仅覆盖已返回 mapping 的最后释放，不覆盖 mapping 创建 future 的 pending/取消；后者依赖后台完成者清理窗口仍未验证或修复。

旧 write-session 成功/abandon API 不恢复；新协议覆盖 public begin/write/prepare、slot 布局、连续分片与完整性、large >1MiB、真实 I/O 失败、Drop 残留、Send future、batch gate 和 mmap prefix append/flush/sharing。新增取消 seam 只暂停在真实 append 完成后或真实 flush 完成后，不冒充 native append 仍在途的动态证据；在途寿命依赖 0.1.2 完成所有者和稳定身份租约。mapping-create pending cancellation 仍未验证。pi_superblock 旧测试不属于本轮迁移范围。

WriteBatch 本轮 pinned file --all-targets --locked --offline 为 48 passed / 0 failed / 0 ignored：16 lib、8 manifest、8 mmap、2 reclaim、3 reconcile、11 write batch。原有 34 项全部保留。公共 seam 三次真实 RED -> GREEN：begin 2 failed -> 2 passed；连续写入 1 failed -> 1 passed；prepare 检查/namespace 2 failed -> 2 passed。file clippy --all-targets -- -D warnings 通过，旧占位私字段已替换为真实资源。workspace lib check 通过，保留既有 43 个 Superblock 警告，不压制。

历史 P2 修复后套件为 51 passed / 0 failed / 0 ignored，保留上述 48 项，public reclaim 从 2 增至 5。真实 RED 成功编译后：空轮 poll_once 返回 Pending，未提前 Drop batch；非空范围错误扩展，报告 (2,2,0,0) 而非 (1,1,0,0)。GREEN 三个公共回归均通过，非空等待 Drop batch 后实际删除及 report、等待取消后的责任保留/重新回收均验证，无 sleep 或 timeout。pinned file clippy -D warnings、fmt check、workspace lib check 通过（既有 43 个 Superblock 警告保留）。此回收等待取消证据不涉及 native append 在途取消。

本次故障模型修订后 pinned file --all-targets --locked --offline 为 57 passed / 0 failed / 0 ignored：17 lib、8 manifest、8 mmap、5 reclaim、3 reconcile、5 recovery、11 write batch。原 51 个测试保留，仅旧 namespace Unsupported/MediaMetadataSync 断言按批准合同改为真实成功。真实 RED：mmap 1 failed、write batch 2 failed、reconcile 2 failed、新 recovery 3 failed；全部成功编译，分别由旧阻塞错误、半尾部 Corrupt 和保护错误分类触发，之后 GREEN。新增 sync 真实失败、Sealed/large 不修复、实际 truncate 后取消回归为补充验收，不宣称这些补充测试独立 RED。取消后拒绝新写入，重开重新 sync 并精确读取已保留前缀。

协议 crate 25 测试通过；workspace lib check、file all-targets clippy -D warnings、fmt check 全通过，保留 43 个既有 Superblock 警告。重开测试释放旧实例后构造合法权威 State，不依赖 Superblock todo；没有强制终止子进程测试、完整 metadata ordering 或日志恢复验收。native append pending cancel 未验；mapping-create pending cancel 未验且不是已证泄漏，不扩展架构。

当前 P1 修复后 pinned file --all-targets --locked --offline：62 passed / 0 failed / 0 ignored（19 lib、8 manifest、8 mmap、5 reclaim、3 reconcile、8 recovery、11 write batch），原 57 项断言不变。公共 zero/partial cleanup -> repeated reconcile -> begin/write slot0 -> prepare -> lease/read 两项真实 RED 均报 object deletion already admitted，修复后 GREEN。新增真正退休 tombstone 保留、删除提交前/真实删除完成后取消不解封、内部 unresolved 状态不盲删三个回归。Unknown 测试是内部未决证据状态前置条件，不是 native Local Unknown 动态证据；取消 seam 不覆盖 native remove pending，不能据取消推断未删除。file Clippy -D warnings、fmt check、workspace lib check 均通过，既有 43 个 Superblock 警告保留。
