STM32 32 导与 E73 BLE
============================

连接与固件
----------

``device_type="stm32"`` 选择 32 导后端。板端 USB CDC 使用
``serial://PORT``；BLE 使用 E73 专用服务，不能套用 ESP32 的八导 BLE key。
发现结果保留设备类型，可直接传给 ``Board.connect``。

当前固件广播名为 ``OmniBCI-32CH``，SDK 也兼容旧名称 ``OmniBCI32-BLE``；
选择实际发现的目标设备，不要只按广播名猜测身份。

.. code-block:: python

   from omnibci import Board

   devices = Board.discover(transport="ble", device_type="stm32", timeout=10)
   if not devices:
       raise RuntimeError("未发现 STM32/E73 BLE 设备")
   with Board.connect(devices[0], timeout=20) as board:
       assert board.get_config().verified
       board.start()
       batch = board.read(250, timeout=5)
       print(batch.eeg_uv.shape)  # (250, 32)
       board.stop()

多台设备时，应根据发现的标识选择目标，不要假定列表顺序或把 MAC 地址当作 key。
连接时驱动会重新扫描以恢复平台设备对象；连接超时包括扫描和握手。

串口连接示例为 ``Board.connect("serial://COM5", device_type="stm32")``，
Linux/macOS 应使用系统提供的串口路径。

要求 E73 SPI 桥接固件；实际 TX/RX 均为 2M，MTU 至少 153。链路自检镜像不能
完成 STM32 配置。STM32 必须支持 32.0.5 系列的 AE/BE 完整配置事务，32.0.4
不兼容此路径。当前固件仅提供板端 USB CDC 与 E73 BLE 直连，已移除 ESB Dongle
和双模式切换；本绑定不自动重连。

``board.info.stm32`` 与 ``board.info.e73`` 分别给出各组件独立报告的版本、Git
revision、dirty、状态和协议；``state`` 为 ``available``、``unsupported``、
``timeout`` 或 ``read_failed``。顶层 ``firmware``、``hardware``、``protocol``
字段继续保留，原有 ESP32 调用不受影响。

配置
----

使用 :py:class:`omnibci.Stm32Config`，与 ESP32 的 FrontendConfig 区分：

.. code-block:: python

   from dataclasses import replace

   config = replace(board.get_config(), gains=(12,) * 32)
   board.configure(config, timeout=5)
   assert board.get_config().verified

字段 ``enabled_mask``、``bias_mask``、``loff_mask`` 为 32 位无符号整数，
bit 0 对应 CH1；BIAS 和 LOFF 必须是 enabled 的子集。默认 enabled/bias
为 ``0xffffffff``，LOFF 为 0，增益为 32 个 24。增益允许 1、2、4、6、8、12、24。
``mode`` 使用 ``both_bias``（默认）、``eeg``、``no_bias``、``shorted``、``test``
字符串，SDK 分别映射到固件编号 0–4；LOFF 仅允许前三种模式。
这里不提供 ESP32 的 SRB2 开关或采样率调整。

只在停采时配置，并等待实际硬件确认。关闭时需停采并确认关闭 LOFF，失败会报告
错误，不能据此认为电极激励已关闭。阻抗测量见下文“阻抗测量”。
``close()`` 先请求停止和断开；若 Disconnect 停止失败而驱动仍在运行，会在有限
宽限期后发送 Shutdown，由 SDK 执行尽力停止并退出，清理失败仍会报告。

阻抗测量
--------

:py:meth:`omnibci.Board.measure_impedance` 完成一次 32 导阻抗测量。它先确认当前
配置并停止采集，临时关闭 BIAS 回授，在选中的已启用通道上开启 31.25 Hz LOFF
激励，采集指定时长（默认 4 秒）后停止，再恢复原配置且保持 LOFF 关闭。整个事务持有 Board 锁；
返回后设备停在 ready 状态，不会自动恢复 EEG，需要时显式调用 ``start()``。

.. code-block:: python

   from omnibci import Board

   with Board.connect("serial://COM5", device_type="stm32", timeout=20) as board:
       result = board.measure_impedance(mask=0x0000FFFF, series_kohm=9.98)
       for channel, value in enumerate(result.values_kohm, start=1):
           print(channel, value, "kΩ" if value is not None else "无效")
       print("削顶通道", hex(result.clipped_mask))
       # 需要继续采集时显式恢复：
       # board.start()

``mask`` 为 32 位通道掩码，与 ``enabled_mask`` 取交集；省略时测量全部已启用
通道。``series_kohm`` 默认 9.98 kΩ，传 0 可得到未补偿原值。结果中未选中、
削顶或有效样本不足的通道为 ``None``。

估计使用 31.25 Hz 正弦、余弦和直流三项最小二乘拟合；ADS1299 的 ±6 nA 交流
激励基波峰值为 ``4·6/π nA``。扣除串联电阻后负值截为 0。窗口固定 4 秒，通道
至少需要 1 秒有限有效样本；窗口内任一有效样本达到 ADS1299 满量程码即拒绝该
通道。相位按设备序号计算，丢包不会压缩载波。该数值是未标定的名义幅值，硬件
校准前不应作为精确电阻。

失败时先尽力停止并恢复原配置（LOFF 关闭）。若无法确认恢复，原异常保留，并以
异常 note 明确指出 LOFF 可能仍在激励；此时关闭 Board，避免继续使用状态未知的
设备。成功恢复后清空测量尾数据，避免后续 EEG 读取返回旧阻抗数据。数据交付
中断达到 2 秒会重新积累窗口，过期数据不参与计算。``timeout`` 仅限制收集时间，
启停、配置与恢复另有各自的有界超时。此功能已做离线算法和故障恢复测试，尚未
做阻抗实机标定。

数据与错误
----------

``eeg_uv`` 和 ``raw_counts`` 分别为只读 float32/int32 的 ``(N,32)`` 数组。
``status`` 为 ``(N,12)``，依次保留四片 ADS1299 的三字节状态。
``sample_rate_hz`` 为 250；无效样本由 ``valid`` 标记，序号缺口保留，不补造 EEG。
原始 V3 包长 128 字节；增益来自已确认配置。

.. code-block:: python

   from pathlib import Path
   from omnibci import decode_frames

   batch = decode_frames(
       Path("capture.bin").read_bytes(), device_type="stm32", gains=(24,) * 32
   )

离线调用不保存末尾半帧，适合完整原始 V3 缓冲区，不能直接输入 BLE 通知封包。
指定的增益必须匹配录制时配置。

配置和启停等待 STM32 执行回执；STOP 还等待 SD 关闭，保留已接收的尾帧。
断开、队列溢出或控制超时明确报告错误，不继续使用未知的硬件状态。
固定长度读取发生异常时，已消费数据通过 ``error.partial`` 提供。
新的连接是新的会话，不自动续接前一次采集。

硬件事件使用 :py:meth:`omnibci.Board.read_triggers` 读取；采集中同时消费
EEG 与事件队列。源时间、回绕、事件关联、32.2.4 时钟就绪条件及完整示例见
:doc:`trigger`，不要用主机接收时间代替设备时间。

验证范围
--------

本分支固定到 ``omnibci-sdk`` 的 ``master`` 分支提交
``e1f25b3a6d272e5040929c53b331d45738ff6d73``。该基线提供 STM32/E73 组件固件信息。
本次 SDK 升级保持公开 Python API 与包版本 0.2.0 不变，只重新构建并运行离线测试，
未重跑新 SDK 的实机采集；具体结果见 ``docs/development.rst``。旧 SDK
``aa9ab512ecf6243646fbbb23e9432e3c79eac1ec`` 下的 STM32 32.2.3 短时实机结果见
:doc:`trigger`，不能作为新固定提交的硬件证据。
``close()`` 返回不表示 Windows 已完成底层 BLE 资源清理；后续连接仍需等待 SDK
清理门控。若系统清理持续阻塞，新连接会报告错误，可能需要重启 Python 进程。
这不提供 Python 会话自动重连，也不代表 BLE ATT ACK 超时根因已经解决。
配套固件与此 Python 接口尚未完成长时实机稳定性验收。
软件测试检查帧解析和会话行为；设备连接、掉线恢复、长时采集必须单独记录实测结果。
运行 ``examples/acquire_stm32.py --help`` 查看显式采集脚本；默认软件测试不访问硬件。

可调阻抗窗口
------------

``window_seconds`` 指定采样窗口时长，默认 4 秒，范围为 ``[1, 60]``；
支持小数，按 ``ceil(采样率 * window_seconds)`` 向上取整到采样点。
它不会先等待稳定再采样，缺失样本仍占时间位置；每通道仍须至少一秒有效数据。
``timeout`` 是独立的数据采集超时（最大 60 秒），不会随窗口自动增加；
请为窗口和数据传输预留余量，设置过短可能超时并触发正常清理恢复。

.. code-block:: python

   result = board.measure_impedance(window_seconds=8.0, timeout=15.0)
