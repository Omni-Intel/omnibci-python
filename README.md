# OmniBCI Python

独立 Rust SDK 的 Python 接口。通过 USB 串口或 BLE 连接 OmniBCI ESP32 + ADS1299 板子，配置前端并采集 8 通道、250 Hz EEG。

## 安装

**Python 3.11+**

从 [Release](https://github.com/Omni-Intel/omnibci-python/releases/latest) 下载匹配平台的 wheel 后安装：

```sh
python -m pip install path/to/omnibci-0.1.2-cp311-abi3-win_amd64.whl
```

串口需要系统识别设备及相应驱动/访问权限。BLE 需要蓝牙适配器；Linux 使用 BlueZ/D-Bus，macOS 需要终端或 Python 宿主的蓝牙权限。关闭其他占用板子的 GUI/程序后再连接。

## USB 采集

```python
from omnibci import Board

print(Board.discover())  # 枚举串口候选

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

# 在返回的候选中选择目标设备
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

BLE endpoint 中的 key 是平台相关的 SDK 设备标识，请使用发现的结果，不要把 MAC 地址当作 key。

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

`stop()` 保留已接收但尚未读取的尾部样本；新的 `start()` 清空上一次采集缓存。Rust 队列有界（128 批），消费过慢导致溢出时会报告故障，不静默覆盖旧数据。

## 超时与错误

```python
from omnibci import PartialReadError

try:
    batch = board.read(250, timeout=2)
except PartialReadError as error:
    partial = error.partial  # 已消费的部分样本，或 None
    # 将 partial 保存/处理
```

所有设备异常派生自 `OmniBCIError`：`DeviceTimeoutError`、`InvalidStateError`、`ConfigurationError`、`DisconnectedError`、`BufferOverflowError`、`TransportError`。`PartialReadError` 派生自 `DeviceTimeoutError`。读取中途的设备故障同样携带 `.partial`；Python 参数错误使用 `ValueError` / `TypeError`。

timeout 单位秒，范围 `(0, 60]`。一次固定长度读取共用一个超时预算。每个 Board 的操作串行执行；预算不包括等待其他调用释放 Board 锁的时间。原生硬件等待释放 GIL，其他 Python 线程可以运行。当前接口为同步阻塞接口，异步程序可用 `asyncio.to_thread`；取消 Python task 不会中断已开始的原生调用。停止和关闭会等待同一 Board 上正在进行的 read 完成或超时。

## 开发

使用 uv 时可运行`uv python install` 和 `uv venv` 创建开发环境；包本身支持 CPython 3.11 及以上版本。

```sh
git clone --recurse-submodules git@github.com:Omni-Intel/omnibci-python.git
cd omnibci-python
git switch master
python -m venv .venv
# 激活 .venv 后
python -m pip install "maturin>=1.15,<2" numpy pytest
maturin develop
python -m pytest -q
cargo fmt --check
cargo clippy --locked -- -D warnings
maturin build --release --locked --out dist
maturin sdist --out dist
```

源码构建需要 Rust/Cargo（建议最新 stable），Windows 需要 MSVC C++ Build Tools；Linux 需要 pkg-config、libudev-dev、libdbus-1-dev。
