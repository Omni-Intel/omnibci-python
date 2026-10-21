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

本次只更新 SDK 固定提交并重新构建，公开 Python API 与包版本 0.2.0 不变，未连接硬件。
``cargo test --locked`` 的 22 个原生 Rust 测试通过；release abi3 扩展重新构建成功，
并通过 66 个 Python 测试（排除硬件测试）。``cargo fmt --check`` 与
``cargo clippy --locked -- -D warnings`` 通过。
未使用新 SDK 重跑实机测试；旧 SDK 基线下的 STM32 32.2.3 结果见
:doc:`trigger`，不能作为新固定提交的硬件证据。

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
