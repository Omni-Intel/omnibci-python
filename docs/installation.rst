安装
====

运行要求
--------

* Python 3.11+。
* NumPy 1.26+（自动安装）。
* USB 串口需要设备驱动及串口访问权限。
* BLE 需要可用的蓝牙适配器；Linux 使用 BlueZ/D-Bus，macOS 需要为终端或 Python
  宿主授予蓝牙权限。

安装 wheel
----------

阻抗接口需使用 ``dev/8ch`` 的开发 wheel 或本分支源码。既有 ``v0.1.2``
Release 不包含该接口；开发包版本仍为 0.1.2，不能仅凭文件名区分。
运行 ``python -c "import omnibci; print(omnibci.SDK_REVISION)"`` 应输出
``dab77683cec6c60fab8244648acb1b5f4514cf09``。

从 `GitHub Releases <https://github.com/Omni-Intel/omnibci-python/releases>`_
下载匹配操作系统、CPU 架构和 Python 的 wheel。以下为 Windows x86-64 示例，
将路径替换为下载文件：

.. code-block:: console

   python -m pip install path/to/omnibci-0.1.2-cp311-abi3-win_amd64.whl
   python -c "import omnibci; print(omnibci.__version__)"

从源码安装
----------

源码构建需要 Rust/Cargo 和包含 SDK 子模块的完整源码。Windows 需要 MSVC C++
Build Tools；Linux 需要 pkg-config、libudev-dev 和 libdbus-1-dev。

.. code-block:: console

   git clone --branch dev/8ch --recurse-submodules https://github.com/Omni-Intel/omnibci-python.git
   cd omnibci-python
   python -m venv .venv

激活虚拟环境后执行：

.. code-block:: console

   python -m pip install "maturin>=1.15,<2" numpy pytest
   maturin develop

已有检出目录使用 ``git submodule update --init --recursive`` 初始化 SDK。
