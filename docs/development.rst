构建与维护文档
==============

本仓库环境
----------

使用本仓库提交的 uv.lock，不借用其他工程环境。需要 Rust stable 与平台构建工具。

.. code-block:: console

   git submodule update --init --recursive
   uv sync --locked --all-extras
   uv run --no-sync maturin develop
   uv run --no-sync python -m pytest -q
   cargo test --locked
   cargo fmt --check
   cargo clippy --locked -- -D warnings
   uv run --no-sync ruff check python tests examples
   uv run --no-sync ruff format --check python tests examples
   uv run --no-sync python -m sphinx -n -W --keep-going -b html docs docs/_build/html
   uv run --no-sync maturin build --release --locked --out dist
   uv run --no-sync maturin sdist --out dist
   uv run --no-sync python tests/check_artifacts.py

更改依赖后用 uv lock 更新锁文件，提交后重新执行 locked 同步。Python 版本可通过
``uv sync --locked --all-extras --python 3.12`` 等显式选择，仍使用本仓库自己的环境。
文档入口为 ``docs/_build/html/index.html``；缺失引用和警告视为失败。

首次慢速下载若触发 maturin 构建隔离的缓存锁超时，可先执行
``uv sync --locked --all-extras --no-install-project``，待工具依赖安装完成后再执行
完整的 ``uv sync --locked --all-extras``。两步使用同一锁文件和项目环境。

SDK 升级验证
------------

2026-10-29 升级到 SDK 0.3.1，固定提交 ``54fbad405c05bfef813f7acb323e5f0c73153092``。公开 Python API 与包版本 0.2.0 保持不变。阻抗测量继续使用停采、配置、采集及恢复流程；此次升级不新增自动型号识别或 live-LOFF 的 Python 参数。

Windows 上 ``cargo test --locked`` 的 28 个原生测试及重新构建扩展后的 173 个 Python 离线测试通过；``cargo fmt --check``、严格 Clippy、Ruff 检查和格式检查通过。release abi3 wheel 与 sdist 构建及内容审计通过；wheel 在独立 Python 3.11 环境安装后，SDK 版本和 revision 与源码一致。固定 SDK 的 176 个单元测试和 10 个集成测试通过。

使用本次构建连接 COM25 STM32 32.3.0 USB 32 导和 COM8 ESP32 dual-stack_v0.25.4 USB 8 导，分别完成配置确认、250 样本读取、数据形状检查、停止和关闭。另通过 ESP32 0.26.1（44:B1:76:47:32:7A）和 STM32 32.3.0 / E73 0.6.0（CD:FB:E9:DC:82:6E）的 BLE 发现、配置确认、250 样本读取、停止和关闭。该短时检查不覆盖阻抗激励、触发器或长时稳定性，也不代表 Linux/macOS 验收。历史触发测试见 :doc:`trigger`。

源码与发布
----------

SDK 子模块必须固定到可从远端获取的提交；只在本机存在的提交不能作为公开构建依赖。
发布前核对 Python/Cargo/runtime 版本、SDK_REVISION、源提交、wheel SHA256 和 CI。
``dev/32ch`` 推送与 PR 执行构建；只有版本 tag 在所有平台通过后发布 GitHub Release。
软件测试不会自动连接或刷写硬件。

实机测试需要用户显式设置 ``OMNIBCI_TEST_ENDPOINT``；STM32 还需设置
``OMNIBCI_TEST_DEVICE_TYPE=stm32``。该测试会开始并停止采集，不是只读检查。
开发固件的 BLE 稳定性边界见 :doc:`stm32`。

维护约定
--------

``conf.py`` 从 ``pyproject.toml`` 读取版本。API 文档与类型声明需同步公开接口；
Sphinx 只验证文档语法和交叉引用，不替代采集测试。

新增公共方法、数据类字段或导出项时，同步 ``api.rst``、专题页与示例；尤其核对
``python/omnibci/__init__.py`` 的导出、实际 Python 签名和 ``_native.pyi``。
本项目使用原生 RST；新增页面必须加入 ``index.rst`` 的 toctree。
``trigger.md`` 仅保留旧链接入口，正文在 :doc:`trigger`，不应在两处维护副本。

故障后的激励清理
----------------

STM32/E73 会话在可能开启 LOFF 时发生故障，会先尝试确认停止，再提交 LOFF=0 配置并等待匹配确认，之后释放连接。后台清理上限为 10 秒，显式关闭还受调用者超时限制。原始故障始终保留，清理不能恢复采集；若连接已断或确认缺失，仍报告激励关闭未确认。回归测试覆盖队列溢出、丢失确认、清理前及清理期间到达的旧 ACK，以及关闭期限。CC 使用 DeepSeek deepseek-flash[1m] 参与审查和实现，Codex 复核并修复 ACK 顺序边界。
