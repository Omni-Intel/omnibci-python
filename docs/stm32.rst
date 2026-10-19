STM32 32 导与 E73 BLE
============================

连接与固件
----------

``device_type="stm32"`` 选择 32 导后端。USB 直连和 ESB dongle 使用
``serial://PORT``；BLE 使用 E73 专用服务，不能套用 ESP32 的八导 BLE key。
发现结果保留设备类型，可直接传给 ``Board.connect``。

.. code-block:: python

   from omnibci import Board

   devices = Board.discover(transport="ble", device_type="stm32", timeout=10)
   if not devices:
       raise RuntimeError("未发现 OmniBCI32-BLE")
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

要求 E73 SPI 桥接镜像，或已处于 BLE 模式的双模镜像；实际 TX/RX 均为 2M，
MTU 至少 153。自检镜像不能完成 STM32 配置。STM32 必须支持 32.0.5 系列的
AE/BE 完整配置事务，32.0.4 不兼容此路径。第一版不提供无线模式切换和自动重连。

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

只在停采时配置，并等待实际硬件确认。LOFF 是激励配置，不是阻抗数值 API；
本包不复刻 GUI 的阻抗估计和 BIAS 临时补偿流程。关闭时需停采并确认关闭 LOFF，
失败会报告错误，不能据此认为电极激励已关闭。``close()`` 先请求停止和断开；
若 Disconnect 停止失败而驱动仍在运行，会在有限宽限期后发送 Shutdown，由 SDK
执行尽力停止并退出，清理失败仍会报告。

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

验证范围
--------

本分支固定到 ``omnibci-sdk`` 的 ``master`` 分支提交
``19eda533909ed833ae7b8106df32a80d9255994c``，与更新后的 GUI 使用同一 SDK 基线。
该基线提供 STM32/E73 组件固件信息，并保留 Windows BLE 后台清理和新连接保护修复。
``close()`` 返回不表示 Windows 已完成底层 BLE 资源清理；后续连接仍需等待 SDK
清理门控。若系统清理持续阻塞，新连接会报告错误，可能需要重启 Python 进程。
这不提供 Python 会话自动重连，也不代表 BLE ATT ACK 超时根因已经解决。
配套固件与此 Python 接口尚未完成长时实机稳定性验收。
软件测试检查帧解析和会话行为；设备连接、掉线恢复、长时采集必须单独记录实测结果。
运行 ``examples/acquire_stm32.py --help`` 查看显式采集脚本；默认软件测试不访问硬件。
