公开 API 参考
=============

.. py:module:: omnibci
   :synopsis: OmniBCI 设备发现、配置与 EEG 采集。

公开接口均从 ``omnibci`` 导入。

Board
-----

.. py:class:: Board(native_device)

   持有单个设备，用 :py:meth:`connect` 创建。支持 ``with``，退出时关闭设备。

   .. py:classmethod:: connect(device: str | DeviceInfo, *, timeout: float = 15.0) -> Board

      连接设备并等待 SDK 确认就绪。

      :param device: ``serial://PORT``、``ble://KEY`` 或设备发现返回的对象。
      :param timeout: 连接超时秒数，范围 ``(0, 60]``。

   .. py:staticmethod:: discover(*, transport: str = "serial", timeout: float = 10.0) -> list[DeviceInfo]

      枚举串口候选或扫描 BLE。``transport`` 只能是 ``"serial"`` 或 ``"ble"``。
      串口枚举不会连接并验证候选设备；timeout 仍会校验，但不用于串口枚举等待。

   .. py:property:: snapshot
      :type: Snapshot

      当前设备状态和采集统计快照。

   .. py:property:: info
      :type: DeviceMetadata | None

      快照中的设备元信息，尚不可用时为 ``None``。

   .. py:method:: get_config() -> FrontendConfig

      返回 SDK 最后确认的硬件配置；没有已确认配置时抛出
      :py:exc:`InvalidStateError`。

   .. py:method:: configure(config: FrontendConfig, *, timeout: float = 3.0) -> None

      发送配置并等待确认。应在停止采集时调用。
      ``config.verified`` 不参与下发。

   .. py:method:: start(*, timeout: float = 5.0) -> None

      等待 SDK 确认开始采集；成功后清除上次采集的 Python 待读缓存。

   .. py:method:: stop(*, timeout: float = 5.0) -> None

      等待 SDK 确认停止采集，保留未读尾部样本。

   .. py:method:: read(samples: int | None = None, *, timeout: float = 2.0) -> SampleBatch

      ``samples=None`` 时返回一批可用样本，否则返回恰好指定数量的已接收样本。
      指定数量必须为 1–1,000,000 的整数，多余样本留待下次读取。

      :raises PartialReadError: 读取超时，``partial`` 提供已消费的样本或 None。
      :raises OmniBCIError: 设备故障；原生读取抛出的设备异常携带部分结果。
      :raises ValueError: 样本数或 timeout 无效。

   .. py:method:: iter_batches(samples: int | None = None, *, timeout: float = 2.0) -> Iterator[SampleBatch]

      持续调用 :py:meth:`read` 的迭代器，每次读取有独立的 timeout 预算。
      使用前需调用 :py:meth:`start`。迭代器不会自动停止采集或捕获异常。

   .. py:method:: close(*, timeout: float = 6.0) -> None

      显式关闭设备并清除 Python 待读缓存。成功关闭后重复调用直接返回。

   .. py:method:: __enter__() -> Board

      检查设备未关闭并返回自身。

   .. py:method:: __exit__(exc_type, exc, tb) -> bool

      调用 ``close()``，返回 False，不吞掉上下文中的异常。
      若已有异常且清理也失败，清理错误作为 note 附加到原异常。

DeviceInfo
----------

.. py:class:: DeviceInfo(id: str, name: str, transport: str, address: str, rssi_dbm: int | None = None)

   设备发现结果，不可变数据类。

   .. py:attribute:: id
      :type: str

      连接使用的设备标识。

   .. py:attribute:: name
      :type: str

      显示名称。

   .. py:attribute:: transport
      :type: str

      ``"serial"`` 或 ``"ble"``。

   .. py:attribute:: address
      :type: str

      设备地址信息，不应假设 BLE 地址就是连接 key。

   .. py:attribute:: rssi_dbm
      :type: int | None

      BLE 信号强度，缺失或串口设备为 None。

   .. py:property:: endpoint
      :type: str

      根据 transport 和 id 生成的连接字符串。

DeviceMetadata
--------------

.. py:class:: DeviceMetadata(firmware: str, hardware: str, protocol: str)

   设备报告的版本信息，不可变数据类。

   .. py:attribute:: firmware
      :type: str

      固件版本信息。

   .. py:attribute:: hardware
      :type: str

      硬件信息。

   .. py:attribute:: protocol
      :type: str

      协议信息。

FrontendConfig
--------------

.. py:class:: FrontendConfig(reference="srb1", mode="eeg", enabled_mask=255, bias_mask=255, srb2_mask=0, gains=(24, 24, 24, 24, 24, 24, 24, 24), verified=False)

   不可变配置数据类。构造时校验参数，无效值抛出 ValueError。
   具体约束和示例见 :doc:`configuration`。

   .. py:attribute:: reference
      :type: str

      ``srb1`` 或 ``srb2``。

   .. py:attribute:: mode
      :type: str

      ``both_bias``、``eeg``、``no_bias``、``shorted`` 或 ``test``。

   .. py:attribute:: enabled_mask
      :type: int

      通道使能掩码。

   .. py:attribute:: bias_mask
      :type: int

      BIAS 通道掩码。

   .. py:attribute:: srb2_mask
      :type: int

      SRB2 通道掩码。

   .. py:attribute:: gains
      :type: tuple[int, ...]

      8 个通道增益。

   .. py:attribute:: verified
      :type: bool

      SDK 返回的配置确认标记。

SampleBatch
-----------

.. py:class:: SampleBatch(eeg_uv, raw_counts, sequence, valid, sample_indices, status, mode, generation, sample_rate_hz, received_at)

   批次数据类，通常由 :py:meth:`Board.read` 或 :py:func:`decode_frames` 返回。
   字段不可重新赋值；SDK 返回的数组只读。手工构造时不会自动冻结或验证传入数组。

   .. py:attribute:: eeg_uv
      :type: NDArray

      float32，形状 ``(N, 8)``，按各通道增益换算的微伏值。

   .. py:attribute:: raw_counts
      :type: NDArray

      int32，形状 ``(N, 8)``，ADS1299 有符号 24 位原始计数。

   .. py:attribute:: sequence
      :type: NDArray

      uint32，形状 ``(N,)``，设备序号。

   .. py:attribute:: valid
      :type: NDArray

      bool，形状 ``(N,)``，固件有效标志；不会自动剔除无效样本。

   .. py:attribute:: sample_indices
      :type: NDArray

      uint64，形状 ``(N,)``，采集内样本索引，保留丢包间隔。

   .. py:attribute:: status
      :type: NDArray

      uint8，形状 ``(N, 3)``，ADS 状态字节。

   .. py:attribute:: mode
      :type: NDArray

      uint8，形状 ``(N,)``，固件模式编号。

   .. py:attribute:: generation
      :type: int

      采集会话代数。

   .. py:attribute:: sample_rate_hz
      :type: int

      采样率，当前为 250 Hz。

   .. py:attribute:: received_at
      :type: float

      主机单调时钟上的近似 SDK 交付时间，合批取最后一批。

   .. py:property:: sample_time_s
      :type: NDArray

      float64，形状 ``(N,)``，由样本索引除以采样率计算的相对时间。

   .. py:method:: __len__() -> int

      支持 ``len(batch)``，返回样本数。

Snapshot
--------

.. py:class:: Snapshot(state, metadata, config, generation, samples, missing_samples, crc_errors, ble, error)

   不可变快照数据类；``ble`` 字典的内容仍可修改。

   .. py:attribute:: state
      :type: str

      SDK 状态名称的小写字符串。

   .. py:attribute:: metadata
      :type: DeviceMetadata | None

      设备信息。

   .. py:attribute:: config
      :type: FrontendConfig | None

      已确认的前端配置。

   .. py:attribute:: generation
      :type: int

      采集会话代数。

   .. py:attribute:: samples
      :type: int

      SDK 样本计数。

   .. py:attribute:: missing_samples
      :type: int

      缺失样本计数。

   .. py:attribute:: crc_errors
      :type: int

      CRC 错误计数。

   .. py:attribute:: ble
      :type: dict[str, int]

      SDK BLE 统计字典。

   .. py:attribute:: error
      :type: str | None

      错误描述，没有错误时为 None。

离线函数
--------

.. py:function:: decode_frames(data: bytes, *, gains: tuple[int, ...] = (24, 24, 24, 24, 24, 24, 24, 24)) -> SampleBatch

   解码完整原始串口缓冲区，不访问硬件。

   :param data: 原始串口协议字节流。
   :param gains: 采集时的 8 个通道增益，用于微伏换算。

   跳过 CRC 错误帧，丢弃末尾不完整帧。
   参数类型错误或配置无效会抛出 TypeError / ValueError。

异常
----

.. py:exception:: OmniBCIError

   继承 Exception。SDK 设备异常的共同基类。

.. py:exception:: DeviceTimeoutError

   继承 :py:exc:`OmniBCIError`，设备操作超时。

.. py:exception:: PartialReadError(partial: SampleBatch | None)

   继承 :py:exc:`DeviceTimeoutError`，读取超时。

   .. py:attribute:: partial
      :type: SampleBatch | None

      本次调用已消费的部分样本，没有时为 None。

.. py:exception:: InvalidStateError

   继承 :py:exc:`OmniBCIError`，设备状态不允许当前操作。

.. py:exception:: ConfigurationError

   继承 :py:exc:`OmniBCIError`，设备配置错误。

.. py:exception:: DisconnectedError

   继承 :py:exc:`OmniBCIError`，设备断开或已关闭。

.. py:exception:: BufferOverflowError

   继承 :py:exc:`OmniBCIError`，接收队列溢出。

.. py:exception:: TransportError

   继承 :py:exc:`OmniBCIError`，传输或底层工作线程错误。

版本常量
--------

.. py:data:: __version__
   :type: str

   Python 包版本。

.. py:data:: SDK_VERSION
   :type: str

   底层 Rust SDK 版本。

.. py:data:: SDK_REVISION
   :type: str

   绑定记录的 Rust SDK Git 提交标识。
