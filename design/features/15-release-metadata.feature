# @design-doc: docs/plans/2026-09-02-block-device-superblock-split-design.md
# @rules: RM-01, RM-02, RM-03, RM-04, RM-05, RM-06, RM-07, RM-08, RM-09, RM-10
Feature: 三 crate 发布元数据与边界
  作为发布维护者
  我希望 package 元数据和发布顺序与批准设计一致
  以便三个 crate 可以独立发布并被正确消费

  Scenario Outline: 两条 workspace 依赖边同时声明 path 和 version
    Given package <consumer> 依赖 <dependency>
    When 检查 workspace dependency
    Then 同时声明 <path> 和 crates.io version <version>

    Examples:
      | consumer | dependency | path | version |
      | pi_file_block_device | pi_block_device | ../pi_block_device | 0.2.0 |
      | pi_superblock | pi_block_device | ../pi_block_device | 0.2.0 |

  Scenario Outline: 每个 crate 使用冻结 package version
    Given crate <crate> 的 package metadata
    When 检查 package version
    Then version 为 <version>

    Examples:
      | crate | version |
      | pi_block_device | 0.2.0 |
      | pi_file_block_device | 0.1.0 |
      | pi_superblock | 0.1.0 |

  @critical
  Scenario Outline: 每个 crate 声明精确发布元数据
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given <crate> 的 Cargo package 与发布文件
    When 校验 crates.io 和 docs.rs 元数据
    Then license=MIT、package archive 含 LICENSE、repository 指向当前 origin 且不执行任何规范化，manifest repository 必须与当前 origin 字符串逐字节精确相等、docs.rs metadata 设置 all-features=true、rust-version=1.85、readme=<readme> 且 changelog=<changelog>

    Examples:
      | crate | readme | changelog |
      | pi_block_device | crates/pi_block_device/README.md | crates/pi_block_device/CHANGELOG.md |
      | pi_file_block_device | crates/pi_file_block_device/README.md | crates/pi_file_block_device/CHANGELOG.md |
      | pi_superblock | crates/pi_superblock/README.md | crates/pi_superblock/CHANGELOG.md |

  Scenario: repository 校验不修改 git 配置
    Given 当前 origin 为 http://ser.yinengyun.com:10082/tech/pi_block_device.git
    When 校验三个 crate 的 package.repository 并执行逐字节精确比较
    Then 不调用 git config、不修改 remote URL 且仅报告 manifest repository 是否与当前 origin 字符串逐字节精确相等

  @critical
  Scenario Outline: 每个 crate package 成功且包内容可解析
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given <crate> 的 package 元数据完整
    When 执行 cargo package 并解析生成的 crate archive
    Then package 成功且 archive 中 Cargo.toml、README、CHANGELOG 和 LICENSE 可解析

    Examples:
      | crate |
      | pi_block_device |
      | pi_file_block_device |
      | pi_superblock |

  Scenario Outline: 每个 crate 在先决依赖可由 registry 解析后 dry-run 成功
    Given <crate> 的 package archive 已成功解析且 <prerequisite>
    When 执行 cargo publish --dry-run
    Then dry-run 成功且依赖解析不引用尚未按顺序发布的 workspace 包

    Examples:
      | crate | prerequisite |
      | pi_block_device | 无 workspace package 发布先决条件 |
      | pi_file_block_device | crates.io 可解析 pi_block_device 0.2.0 |
      | pi_superblock | crates.io 可解析 pi_block_device 0.2.0 |

  Scenario: dry-run 按依赖先决顺序执行
    Given 三个 crate 均已完成 cargo package
    When 执行 publish dry-run 计划
    Then 先验证 pi_block_device，再在 registry 可解析其 0.2.0 后验证 pi_file_block_device 和 pi_superblock

  @critical
  Scenario: 发布顺序遵守依赖方向
    # @impl: (TDD GREEN 阶段回填真实测试路径)
    Given 三个 crate 均通过 Rust 1.85 的 check、test、clippy、doc 和 package
    When 执行发布计划
    Then 顺序为 pi_block_device、pi_file_block_device、pi_superblock

  Scenario: 发布说明声明无旧布局迁移
    Given 三个 crate 的 release notes 已生成
    When 检查兼容性声明
    Then 明确声明 pi_block_device 0.1 持久数据无迁移且本版为新布局

  Scenario Outline: 各 crate release notes 声明自身交付边界
    Given <crate> 的 release notes 已生成
    When 检查该 crate 的发布说明
    Then 包含 <claims>

    Examples:
      | crate | claims |
      | pi_block_device | pre-1.0 breaking Rust API、fixed block protocol only、no legacy aggregate facade |
      | pi_file_block_device | physical file backend only、no allocation transaction log、delayed empty-Active materialization |
      | pi_superblock | static device registry、single metadata-log authority、new layout only and no pi_block_device 0.1 persisted-data migration |
