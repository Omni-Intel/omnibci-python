公开 API 参考
=============

.. py:module:: omnibci
   :synopsis: OmniBCI 设备发现、配置与 EEG 采集。

公开接口均从 ``omnibci`` 导入。

Board
-----

.. py:class:: Board(native_device, device_type="esp32")

   持有单个设备，用 :py:meth:`connect` 创建。支持 ``with``，退出时关闭设备。

   .. py:classmethod:: connect(device: str | DeviceInfo, *, timeout: float = 15.0, device_type: str | None = None) -> Board

      连接设备并等待 SDK 确认就绪。

      :param device: ``serial://PORT``、``ble://KEY`` 或设备发现返回的对象。
      :param timeout: 连接超时秒数，范围 ``(0, 60]``。
      :param device_type: ``"esp32"`` 或 ``"stm32"``；省略时使用 DeviceInfo 中的类型，字符串入口默认为 ESP32。

   .. py:staticmethod:: discover(*, transport: str = "serial", timeout: float = 10.0, device_type: str = "esp32") -> list[DeviceInfo]

      枚举串口候选或扫描 BLE。``transport`` 只能是 ``"serial"`` 或 ``"ble"``。
      串口枚举不会连接并验证候选设备；timeout 仍会校验，但不用于串口枚举等待。

   .. py:property:: snapshot
      :type: Snapshot

      当前设备状态和采集统计快照。

   .. py:property:: info
      :type: DeviceMetadata | None

      快照中的设备元信息，尚不可用时为 ``None``。

   .. py:method:: get_config() -> FrontendConfig | Stm32Config

      返回 SDK 最后确认的硬件配置；没有已确认配置时抛出
      :py:exc:`InvalidStateError`。

   .. py:method:: configure(config: FrontendConfig | Stm32Config, *, timeout: float = 3.0) -> None

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

   .. py:method:: measure_impedance(*, mask: int | None = None, series_kohm: float | None = None, timeout: float = 15.0) -> ImpedanceResult

      在 STM32 上完成一次 32 导阻抗测量：确认配置、停止采集、关闭 BIAS 回授、
      在选中通道开启 31.25 Hz LOFF、采集 4 秒、停止并恢复原配置（LOFF 关闭）。
      整个事务持有 Board 锁；返回后设备处于 ready，不自动恢复 EEG。

      :param mask: 32 位通道掩码，与 ``enabled_mask`` 取交集；省略时使用全部已启用通道。
      :param series_kohm: 串联电阻补偿，默认 9.98 kΩ，有效范围 ``[0, 1000]``。
      :param timeout: 采集 4 秒窗口的最长时间，范围 ``(0, 60]``。
      :raises ValueError: 参数无效，或 mask 未选中任何已启用通道。
      :raises ConfigurationError: 非 STM32 设备，或模式不允许 LOFF 激励。
      :raises InvalidStateError: 配置未确认，或设备状态不允许测量。
      :raises OmniBCIError: 停止、采集或恢复失败。无法确认 LOFF 关闭时，原异常
         附带说明该硬件状态未确认的 note，并关闭 Board。

      估计方法、样本要求和限制见 :doc:`stm32` 的“阻抗测量”一节。

   .. py:method:: iter_batches(samples: int | None = None, *, timeout: float = 2.0) -> Iterator[SampleBatch]

      持续调用 :py:meth:`read` 的迭代器，每次读取有独立的 timeout 预算。
      使用前需调用 :py:meth:`start`。迭代器不会自动停止采集或捕获异常。

   .. py:method:: close(*, timeout: float = 6.0) -> None

      显式关闭设备并清除 Python 待读缓存。成功关闭后重复调用直接返回。

   .. py:method:: read_triggers() -> tuple[TriggerEvent, ...]

      取走当前已排队的 STM32 硬件事件，不等待新事件；没有事件时返回空元组。
      与 EEG 队列独立，应在采集中定期轮询。事件可能晚于锚点样本到达，需按
      generation 和 sequence 关联，见 :doc:`trigger`。

      事件队列容量为 16,384；溢出使采集故障，已保留事件仍可排空，之后的空事件
      读取报告 :py:exc:`BufferOverflowError`。STOP 保留尾事件，成功 START
      清空上一代事件。ESP32 调用报告 :py:exc:`ConfigurationError`。
      本方法仍需等待同一 Board 的操作锁，“不等待”不代表可抢占正在进行的 read。

   .. py:method:: __enter__() -> Board

      检查设备未关闭并返回自身。

   .. py:method:: __exit__(exc_type, exc, tb) -> bool

      调用 ``close()``，返回 False，不吞掉上下文中的异常。
      若已有异常且清理也失败，清理错误作为 note 附加到原异常。

DeviceInfo
----------

.. py:class:: DeviceInfo(id: str, name: str, transport: str, address: str, rssi_dbm: int | None = None, device_type: str = "esp32")

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

ComponentFirmware
-----------------

.. py:class:: ComponentFirmware(version: str | None = None, revision: str | None = None, dirty: bool | None = None, state: str = "", protocol: str = "")

   单个组件独立报告的固件来源信息，不可变数据类。
   由 :py:attr:`DeviceMetadata.stm32` 或 :py:attr:`DeviceMetadata.e73` 提供。

   .. py:attribute:: version
      :type: str | None

      组件固件版本；不可用时为 None。

   .. py:attribute:: revision
      :type: str | None

      组件报告的 Git revision；未知或不可用时为 None。

   .. py:attribute:: dirty
      :type: bool | None

      工作区是否有未提交修改；未知或不可用时为 None。

   .. py:attribute:: state
      :type: str

      ``available``、``unsupported``、``timeout`` 或 ``read_failed``。

   .. py:attribute:: protocol
      :type: str

      该组件报告的协议信息。

DeviceMetadata
--------------

.. py:class:: DeviceMetadata(firmware: str, hardware: str, protocol: str, stm32: ComponentFirmware | None = None, e73: ComponentFirmware | None = None)

   设备报告的版本信息，不可变数据类。顶层字段保留原有 ESP32 含义；
   ``stm32`` 和 ``e73`` 分别提供各组件独立报告的信息。

   .. py:attribute:: firmware
      :type: str

      固件版本信息。

   .. py:attribute:: hardware
      :type: str

      硬件信息。

   .. py:attribute:: protocol
      :type: str

      协议信息。

   .. py:attribute:: stm32
      :type: ComponentFirmware | None

      STM32 组件信息；非 STM32 设备或尚未报告时为 None。

   .. py:attribute:: e73
      :type: ComponentFirmware | None

      E73 组件信息；USB 或非 E73 连接为 None。

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

Stm32Config
-----------

.. py:class:: Stm32Config(mode="both_bias", enabled_mask=4294967295, bias_mask=4294967295, loff_mask=0, gains=(24,) * 32, verified=False)

   STM32 32 导不可变配置。mode 使用 ``both_bias``、``eeg``、``no_bias``、
   ``shorted`` 或 ``test`` 字符串，掩码为 32 位整数；BIAS/LOFF 必须是 enabled
   的子集，LOFF 只用于前三种模式。
   gains 为 32 个允许的 PGA 增益，verified 表示实际硬件确认，不参与下发。
   用法与固件限制见 :doc:`stm32`。

ImpedanceResult
---------------

.. py:class:: ImpedanceResult(mask, series_kohm, values_kohm, clipped_mask)

   :py:meth:`Board.measure_impedance` 返回的不可变结果。

   .. py:attribute:: mask
      :type: int

      实际使用的 32 位 LOFF 通道掩码，已与 ``enabled_mask`` 取交集。

   .. py:attribute:: series_kohm
      :type: float

      本次补偿使用的串联电阻，单位 kΩ。

   .. py:attribute:: values_kohm
      :type: tuple[float | None, ...]

      32 个通道的阻抗，单位 kΩ。未选中、削顶或有效样本不足 1 秒的通道为 None。

   .. py:attribute:: clipped_mask
      :type: int

      窗口内出现 ADS1299 满量程码的已选通道掩码。

SampleBatch
-----------

.. py:class:: SampleBatch(eeg_uv, raw_counts, sequence, valid, sample_indices, status, mode, generation, sample_rate_hz, received_at, source_timestamp_us=None, source_timestamp_valid=None)

   批次数据类，通常由 :py:meth:`Board.read` 或 :py:func:`decode_frames` 返回。
   字段不可重新赋值；SDK 返回的数组只读。手工构造不会冻结或校验其他数组的类型
   和形状；两个源时间数组校验长度，省略时生成等长零值和全 False 掩码。

   .. py:attribute:: eeg_uv
      :type: NDArray

      float32，形状 ``(N, 8)`` 或 STM32 的 ``(N, 32)``，按各通道增益换算的微伏值。

   .. py:attribute:: raw_counts
      :type: NDArray

      int32，形状 ``(N, 8)`` 或 STM32 的 ``(N, 32)``，ADS1299 有符号 24 位原始计数。

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

      uint8，形状 ``(N, 3)`` 或 STM32 的 ``(N, 12)``，ADS 状态字节。

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

   .. py:attribute:: source_timestamp_us
      :type: NDArray

      uint32，形状 ``(N,)``，STM32 DRDY 设备微秒时间的低 32 位；不可用时为 0。
      必须结合 source_timestamp_valid 判断，0 可以是有效回绕值。
      相邻差值按模 ``2^32`` 计算；不是主机时钟或 UTC。ESP32 当前不提供该时间。

   .. py:attribute:: source_timestamp_valid
      :type: NDArray

      bool，形状 ``(N,)``，标记对应源时间是否可用；ESP32 当前全为 False。
      由 SDK 返回时，两个源时间数组与其他样本数组一样只读，并随合批、切片保留。

   .. py:method:: __len__() -> int

      支持 ``len(batch)``，返回样本数。

TriggerTiming
-------------

.. py:class:: TriggerTiming(trigger_timestamp_us, anchor_timestamp_us)

   不可变数据类。Python 整数保留设备时间的全部位数，字段单位均为微秒。

   .. py:attribute:: trigger_timestamp_us
      :type: int

      触发包起始位 ISR 的 uint64 设备时间，不是主机发送、到达或刺激呈现时间。

   .. py:attribute:: anchor_timestamp_us
      :type: int

      首个不早于触发起始时刻的实际 EEG DRDY 时间低 32 位（uint32）。
      与触发时间低 32 位做回绕运算，不能直接把 64 位触发值减去截断锚点。

TriggerEvent
------------

.. py:class:: TriggerEvent(generation, code, sequence, source_timestamp_ms, timing)

   STM32 硬件事件，不可变数据类，由 :py:meth:`Board.read_triggers` 或
   :py:func:`decode_triggers` 返回。构造参数均必填，可选字段显式传 None。

   .. py:attribute:: generation
      :type: int

      uint64 采集代数；离线解码为 0，不代表现场实时会话。

   .. py:attribute:: code
      :type: int

      保留协议的 uint16 事件码；当前外部串口发送协议使用 0～255，含 0 和重复码。

   .. py:attribute:: sequence
      :type: int

      锚点 EEG 样本的 uint32 序号；一个锚点可关联多个事件。

   .. py:attribute:: source_timestamp_ms
      :type: int | None

      兼容旧接口的毫秒设备时间低 32 位；不可用时为 None。

   .. py:attribute:: timing
      :type: TriggerTiming | None

      微秒触发时间及 DRDY 锚点；不可用时为 None。不要从主机收包时间伪造。

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
      :type: FrontendConfig | Stm32Config | None

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

.. py:function:: decode_frames(data: bytes, *, gains: tuple[int, ...] | None = None, device_type: str = "esp32") -> SampleBatch

   解码完整原始串口缓冲区，不访问硬件。

   :param data: 原始串口协议字节流。
   :param gains: 采集时的 8 或 32 个通道增益；省略时按设备类型使用每通道 24。
   :param device_type: ``"esp32"`` 或 ``"stm32"``，选择对应的原始串口协议。

   跳过 CRC 错误帧，丢弃末尾不完整帧。
   参数类型错误或配置无效会抛出 TypeError / ValueError。

.. py:function:: decode_triggers(data: bytes) -> tuple[TriggerEvent, ...]

   离线解码完整 STM32 原始协议缓冲区中的硬件事件，不访问设备。
   每次创建独立解析器；CRC 错误包与末尾不完整包不返回，半包不跨调用保留。
   返回 generation 为 0 的事件，源时间保持整数精度；不能直接输入 BLE 通知信封。

触发发送
--------

.. py:class:: TriggerController(port: str, *, timeout: float = 1.0)

   持有外部发送板的串口，使用 10000 baud、8N1、无流控和三字节二进制协议。
   可直接构造或用 :py:meth:`TriggerController.connect` 创建，支持 with。
   端口或 timeout 参数无效（包括类型非法）时报告 ValueError；底层 I/O
   失败报告 :py:exc:`TransportError`。不提供设备接收确认或实时调度保证。

   .. py:classmethod:: connect(port: str, *, timeout: float = 1.0) -> TriggerController

      打开实际发送板串口，如 COM23；它与采集板 Data CDC/DFU 端口不同。
      timeout 是有限秒数，范围 ``(0, 60]``，不接受 bool；用于串口 I/O。

   .. py:staticmethod:: available_ports() -> list[str]

      枚举串口候选，不验证哪个属于发送板。枚举失败报告 TransportError。

   .. py:property:: is_open
      :type: bool

      是否仍持有打开的串口，不表示设备在线或收到事件。

   .. py:method:: send(code: int) -> None

      code 必须是 Python int 且在 0～255 内；拒绝 bool 等类型及越界值，报告
      ValueError。发送成功只确认主机写入/flush。关闭后发送、写入或 flush
      失败报告 TransportError；I/O 失败后底层串口关闭，重试需重新连接。

   .. py:method:: close() -> None

      释放串口，可重复调用。正常 I/O 清理由 SDK 执行，内部锁故障仍可报告
      TransportError。

   .. py:method:: __enter__() -> TriggerController

      返回自身，不重新打开已关闭的控制器。

   .. py:method:: __exit__(exc_type, exc_value, traceback) -> None

      调用 close，不抑制实验异常；与 Board 的退出实现不同，未附加清理错误 note。

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
