# OmniBCI Python

独立 Rust SDK 的 Python 绑定。直接通过 USB 串口或 BLE 连接 OmniBCI ESP32 + ADS1299 板子，配置前端并采集 8 通道、250 Hz EEG。协议解析、设备状态机、CRC、BLE 重传和采集线程均由 `omnibci-sdk` 实现；本仓库只负责 Python 接口、NumPy 转换和打包，不依赖 GUI 或 services。

仓库主分支：`master`。SDK 以 Git 子模块固定版本，不跟随远端分支自动升级。

## 安装

Python 3.11+（标准 CPython，非 free-threaded 版本）。发行包名和导入名均为 `omnibci`。

```sh
# 从 GitHub Release 下载匹配平台的 wheel 后安装，不需要 Rust
python -m pip install path/to/omnibci-0.1.2-cp311-abi3-win_amd64.whl
```

仓库公开且对应版本的 Release 已发布后，也可以让 pip 从该 Release 的附件中按平台选择 wheel：

```sh
python -m pip install "numpy>=1.26"
python -m pip install --no-index --no-deps --only-binary=:all: --find-links https://github.com/Omni-Intel/omnibci-python/releases/expanded_assets/v0.1.2 "omnibci==0.1.2"
```

`--find-links` 读取指定版本的 Release 附件页，`--no-index` 保证 `omnibci` 只从该页获取；因此先单独安装 NumPy。GitHub 的 `expanded_assets` 地址可供 pip 读取，但不是正式的 Python 包索引接口，若 GitHub 调整该页面，请改为下载并安装对应 wheel。`--index-url` 需要符合 Python Simple API 的索引，不能直接指向 GitHub Release 页面。

推送 `vX.X.X` 标签后，CI 会在测试通过时把 wheel 发布到 [GitHub Releases](https://github.com/Omni-Intel/omnibci-python/releases)，不会发布到 PyPI。Release 自带 GitHub 生成的源码 ZIP/TAR.GZ；CI 另行构建并测试 Python 源码发行包，但不上传为 Release 附件。GitHub 自动源码包不含 `sdk` 子模块的文件，不能直接代替可构建的 Python 源码发行包；需要从源码安装时请使用 `git clone --recurse-submodules`。构建目标包括 Windows x64 / ARM64、Linux x64 / ARM64（manylinux_2_28）和 macOS Intel / Apple Silicon；实际构建状态以 Actions 结果为准。ABI3 wheel 面向 CPython 3.11+，CI 在六个平台验证 3.11 / 3.14。不提供 PyPy、32 位或 free-threaded wheel。

串口需要系统识别设备及相应驱动/访问权限。BLE 需要蓝牙适配器；Linux 使用 BlueZ/D-Bus，macOS 需要终端或 Python 宿主的蓝牙权限。关闭其他占用板子的 GUI/程序后再连接。

## USB 采集

```python
from omnibci import Board

print(Board.discover())  # 只枚举串口候选，不会逐个打开或验证设备

with Board.connect("serial://COM5") as board:
    # Linux: serial:///dev/ttyACM0; macOS: serial:///dev/cu.usbmodem...
    print(board.info)
    print(board.get_config())
    board.start()
    batch = board.read(250, timeout=3)
    print(batch.eeg_uv.shape)  # (250, 8)，单位微伏
    print(batch.sample_indices)
    board.stop()
```

`with` 在正常退出和发生异常时均关闭设备。`connect()` 等待 SDK 确认设备就绪；`start()` / `stop()` / `configure()` 等待 SDK 状态或配置确认，失败会抛出异常。不要依赖垃圾回收释放板子。

## BLE 采集

```python
from omnibci import Board

for device in Board.discover(transport="ble", timeout=10):
    print(device.name, device.endpoint, device.rssi_dbm)

# 在返回的候选中选择目标设备，随后传入 DeviceInfo：
devices = Board.discover(transport="ble", timeout=10)
if not devices:
    raise RuntimeError("未发现 BLE 板子")
with Board.connect(devices[0], timeout=20) as board:
    board.start()
    for _ in range(10):
        batch = board.read(250, timeout=5)
        print(batch.eeg_uv.mean(axis=0))
    board.stop()
```

BLE endpoint 中的 key 是平台相关的 SDK 设备标识，请使用发现结果，不要自行把 MAC 地址当作 key。采集期间，ACK/NACK 和重传由 Rust 接收线程处理，不依赖 Python 消费速度。

## 配置

```python
from dataclasses import replace
from omnibci import Board

with Board.connect("serial://COM5") as board:
    config = replace(board.get_config(), gains=(12,) * 8)
    board.configure(config)
    assert board.get_config().verified
```

在停止采集时配置。`FrontendConfig` 字段：

| 字段 | 含义 |
| --- | --- |
| `reference` | `srb1` / `srb2` |
| `mode` | `both_bias` / `eeg` / `no_bias` / `shorted` / `test` |
| `enabled_mask` | 通道使能，bit 0 对应第 1 通道 |
| `bias_mask` | BIAS 通道选择，必须是已启用通道的子集 |
| `srb2_mask` | SRB2 通道选择，必须是已启用通道的子集；SRB1 模式下可保留非零选择，实际开关由参考模式控制 |
| `gains` | 8 个增益，允许 1、2、4、6、8、12、24 |
| `verified` | SDK 返回的硬件配置确认标记，发送配置时忽略此字段 |

Python 提前检查参数；Rust SDK 仍执行协议和设备状态验证。

## 数据与读取语义

`read()` 返回当前可用的一批；`read(n)` 返回恰好 n 个**收到的样本**，多出的样本保留供下次读取。`iter_batches(n)` 是连续调用 `read(n)` 的迭代器，使用前需调用 `start()`。读取有界，`n` 范围为 1–1,000,000。

| `SampleBatch` 字段 | 类型 / 含义 |
| --- | --- |
| `eeg_uv` | float32，`(N, 8)`，按每通道增益换算的微伏 |
| `raw_counts` | int32，`(N, 8)`，ADS1299 有符号 24 位原始值 |
| `sequence` | uint32，`(N,)`，设备序号 |
| `valid` | bool，`(N,)`，固件有效标志；不会自动剔除无效行 |
| `sample_indices` | uint64，`(N,)`，本次采集内的序号时间轴，丢包留下间隔 |
| `status` | uint8，`(N, 3)`，ADS 状态字节 |
| `mode` | uint8，`(N,)`，固件模式编号 |
| `generation` | 每次成功开始采集后的会话代数 |
| `sample_rate_hz` | 250 |
| `sample_time_s` | 由 sample_indices / sample_rate_hz 计算的相对时间 |
| `received_at` | 主机 `time.monotonic()` 时间轴上的近似 SDK 交付时间；合批时取最后一批，非硬件采样时间 |

不对缺失样本填零、不插值、不滤波。CRC 错误帧由 SDK 丢弃，可通过 `board.snapshot` 的 `crc_errors` / `missing_samples` 和序号观察质量。数组有独立的不可变字节 backing，在设备关闭或批次对象释放后仍然有效；需要修改时用 `.copy()`。这不是端到端零拷贝实现。

`stop()` 保留已接收但尚未读取的尾部样本；新的 `start()` 清空上一次采集缓存。Rust 队列有界（128 批），消费过慢导致溢出时会报告故障，不静默覆盖旧数据。

## 超时与错误

```python
from omnibci import PartialReadError

try:
    batch = board.read(250, timeout=2)
except PartialReadError as error:
    partial = error.partial  # 已消费的部分样本，或 None
    # 将 partial 保存/处理；后续 read 不会重复返回这些样本。
```

所有设备异常派生自 `OmniBCIError`：`DeviceTimeoutError`、`InvalidStateError`、`ConfigurationError`、`DisconnectedError`、`BufferOverflowError`、`TransportError`。`PartialReadError` 派生自 `DeviceTimeoutError`。读取中途的设备故障同样携带 `.partial`；Python 参数错误使用 `ValueError` / `TypeError`。

timeout 单位秒，范围 `(0, 60]`。一次固定长度读取共用一个超时预算。每个 Board 的操作串行执行；预算不包括等待其他调用释放 Board 锁的时间。原生硬件等待释放 GIL，其他 Python 线程可以运行。当前接口为同步阻塞接口，异步程序可用 `asyncio.to_thread`；取消 Python task 不会中断已开始的原生调用。停止和关闭会等待同一 Board 上正在进行的 read 完成或超时。

## 开发与验证

仓库的 `.python-version` 将 uv 默认解释器设为 Python 3.11。使用 uv 时可运行
`uv python install` 和 `uv venv` 创建开发环境；包本身支持 CPython 3.11 及以上版本。

```sh
git clone --recurse-submodules git@github.com:Omni-Intel/omnibci-python.git
cd omnibci-python
git switch master
python -m venv .venv
# 激活 .venv 后：
python -m pip install "maturin>=1.15,<2" numpy pytest
maturin develop
python -m pytest -q
cargo fmt --check
cargo clippy --locked -- -D warnings
maturin build --release --locked --out dist
maturin sdist --out dist
```

源码构建需要 Rust/Cargo（建议最新 stable），Windows 需要 MSVC C++ Build Tools；Linux 需要 pkg-config、libudev-dev、libdbus-1-dev。源码发行包内包含固定版本 SDK 源码，无需访问 SDK Git 仓库；Cargo 构建依赖仍需下载或缓存。修改 SDK 时先在 SDK 仓库提交，再更新本仓库的 submodule 指针和 `SDK_REVISION` 常量，运行测试。

默认测试使用真实原生扩展做协议解码，并用确定性设备替身验证 Python 读取/异常逻辑，不会主动连接硬件。显式设置环境变量 `OMNIBCI_TEST_ENDPOINT=serial://COM5` 后运行 `python -m pytest -m hardware` 会启动实板采集；BLE 也可使用发现得到的 endpoint。

`decode_frames(bytes, gains=...)` 可离线解码完整串口缓冲区，复用 Rust SDK。它不是增量解析器，每次调用独立，末尾不完整帧不会保留到下一次调用。

CI 将生成 wheel artifact，并构建、安装测试源码包；wheel 在 Python 3.11 / 3.14 上安装测试。若 SDK 仓库私有，在 Python 仓库配置具有 SDK 只读权限的 `SUBMODULES_READ_TOKEN` secret；默认 `GITHUB_TOKEN` 无法读取其他私有仓库。

## GitHub Release 发布

发布前把 `pyproject.toml` 的 `project.version`、`Cargo.toml` 的 `package.version` 和 `python/omnibci/__init__.py` 的 `__version__` 改为同一个 `X.X.X`，运行 `cargo check` 更新 `Cargo.lock`，并提交这些文件。然后在该提交上创建并推送 `vX.X.X` 标签，例如 `v0.1.3`。CI 会检查标签与这三个版本一致，完成各平台构建、安装测试和源码包测试后创建 GitHub Release，仅附上六个平台的 wheel。标签不符合格式、版本不一致或缺少任何 wheel 时不会发布。重新运行同一标签的工作流会更新该 Release 的附件。

发布前还应完成目标板 USB/BLE 实测；CI 的自动测试不连接硬件。

## 许可证

本项目采用 BSD-3-Clause 许可证，见 [LICENSE](LICENSE)。设备 SDK 是独立的 Git 子模块，许可条款见其 [LICENSE](sdk/LICENSE)。
