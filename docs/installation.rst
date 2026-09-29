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

   git clone --recurse-submodules https://github.com/Omni-Intel/omnibci-python.git
   cd omnibci-python
   git switch master
   python -m venv .venv

激活虚拟环境后执行：

.. code-block:: console

   python -m pip install "maturin>=1.15,<2" numpy pytest
   maturin develop

已有检出目录使用 ``git submodule update --init --recursive`` 初始化 SDK。
