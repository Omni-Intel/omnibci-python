硬件触发接收与发送
==================

本页对应当前源码，SDK 固定提交为
``54fbad405c05bfef813f7acb323e5f0c73153092``。
STM32 采集使用板端 Data CDC 或 E73 BLE，当前固件已移除 ESB Dongle 和模式切换。
外部发送板的串口由 :py:class:`omnibci.TriggerController` 独立持有，
与采集板的 Data CDC、DFU CDC 均不是同一个端口。安装入口见 :doc:`installation`。

事件、样本与源时间
------------------

:py:meth:`omnibci.Board.read_triggers` 不等待新事件，取走当前已排队的 STM32
事件。每个不可变 :py:class:`omnibci.TriggerEvent` 包含采集代数 generation、
事件码 code、锚点 sequence、旧毫秒 source_timestamp_ms 和可选 timing。
:py:class:`omnibci.TriggerTiming` 保留 uint64 trigger_timestamp_us 和
uint32 anchor_timestamp_us；Python 整数保留全部位数。

触发时间是包头起始位 ISR 的设备时间，不是主机发送、到达、屏幕显示或刺激实际
呈现时间。锚点是第一个不早于它的实际 EEG DRDY 样本；同一锚点可以有多个事件，
重复码和 code=0 均须分别保留。按 generation 与 sequence 关联 EEG，
事件可能在后续 read 批次才到达，不能按循环次数或主机到达时间配对。

样本提供 source_timestamp_us（uint32 DRDY 微秒时间低 32 位）和
source_timestamp_valid 掩码。不可用时值为 0、掩码为 False；0 配合 True
是合法回绕值。SDK 0.3.1 也保留 ESP32 时间字段：0.26.1 为 DRDY 中断入口时间，更早的 serial-v1 固件为组帧时间；可用掩码不保证时间语义。固定长度读取、切片、合批和离线
解码保留这两个数组；SDK 返回的数组只读。

相邻时间戳差值按模 ``2^32`` 计算，例如
``delta_us = (int(current) - int(previous)) & 0xffffffff``。
触发偏移要将 trigger_timestamp_us 的低 32 位与锚点做有符号回绕差；不能
直接用完整 uint64 减去截断的 uint32，也不要把事件取整到 EEG 的整数样点网格。
SDK 只提供源时间，BDF+ 写入与录制时间轴映射由调用方完成。

32.2.4 时钟前置条件
-------------------

STM32 32.2.4 在启动稳定后将 EEG 与触发时间统一到 ADS 参考的单调时钟，
字段和单位不变。时序实验前连续采集至少一秒，并通过独立 DFU CDC 的
``mcumgr stat clock`` 确认 ``ready=1``。Board 没有此诊断的查询 API，
仅等待一秒不能证明已稳定；此时间也不代表 UTC 或主机时钟校准。
启动偏移、参考丢失、ISR 延迟和物理精度限制见
`固件时钟契约 <https://github.com/Omni-Intel/omnibci-firmware-stm32/blob/main/stm32/TIMING.md>`_。

双队列与关闭边界
----------------

EEG 与事件队列独立，采集中必须持续消费两者。事件队列容量为 16,384；
溢出使采集故障，已保留事件仍可排空，之后的空事件读取报告
:py:exc:`omnibci.BufferOverflowError`。STOP 保留尾事件，成功 START 清空
上一代事件。关闭或再次开始前，应排空所需尾部 EEG 和事件。
ESP32 硬件事件轮询报告 :py:exc:`omnibci.ConfigurationError`，
它原有的样本采集路径不受影响。

``read_triggers()`` 仍受 Board 操作锁约束；它不等待新事件，但不能抢占正在
执行的 read。避免用长时间、大样本数的阻塞读取妨碍事件消费。

最小 USB 发送与接收示例
-----------------------

以下代码需要真实 STM32 与外部发送板，端口必须替换为实际值。它只说明 API
和双队列处理，不是时序精度或实时调度验收；时序实验需另外完成上面的就绪检查。
长实验应增量存盘或处理，示例只在内存保留约五秒数据。

.. code-block:: python

   import time
   from omnibci import Board, DeviceTimeoutError, TriggerController

   batches = []
   events = []
   with Board.connect("serial://COM5", device_type="stm32") as board:
       with TriggerController.connect("COM23", timeout=1.0) as trigger:
           board.start()
           try:
               trigger.send(7)  # 主机写入成功，接收确认要看 events
               deadline = time.monotonic() + 5
               while time.monotonic() < deadline:
                   batches.append(board.read(25, timeout=1))
                   events.extend(board.read_triggers())
           finally:
               board.stop()

           # 停止后保留尾数据；只把空 EEG 队列的超时视为排空结束。
           while True:
               try:
                   batches.append(board.read(timeout=0.1))
               except DeviceTimeoutError:
                   if board.snapshot.error is not None:
                       raise  # 设备故障超时不能当作正常排空结束
                   break
               events.extend(board.read_triggers())
           events.extend(board.read_triggers())

   print(sum(len(batch) for batch in batches), "samples;", len(events), "events")
   for event in events:
       print(event.generation, event.code, event.sequence, event.timing)

发送端接受 Python int 的 0～255（含 0、重复码），拒绝 bool 等其他类型。
协议为 10000 baud、8N1 的 A5/code/complement，发送或 flush 失败后串口关闭。
with 退出会释放端口，重试需重新连接。不要用发送调用时间替代设备源时间。
BLE 只需按 :doc:`stm32` 的发现与连接方式替换采集端，发送板仍使用自己的串口。

离线解码
--------

:py:func:`omnibci.decode_triggers` 解码完整 STM32 原始协议缓冲区，返回
generation=0 的事件。每次创建独立解析器；CRC 错误和末尾半包不返回，
半包不跨调用保留。不能直接传入带 BLE 信封的通知；实时采集使用 Board。

验证基线与限制
--------------

2026-10-21 的历史 Windows 实机验收使用 STM32 32.2.3、E73 0.5.1 和旧 SDK
``aa9ab512ecf6243646fbbb23e9432e3c79eac1ec``。USB、BLE 各接收 1,000/1,000
事件，EEG 缺口、CRC 和源时间/锚点不匹配均为零。Linux/macOS 未实测；
当时没有 ESB Dongle 硬件，该路径现已退役。这只是短时结果，不验证当前 pin。
原始本机证据位于 ``D:/workspace/trigger-validation/cc-bindings``，
其他机器无法通过此路径访问。

当前 SDK 0.3.1 的构建、软件测试及 USB/BLE 采集验证见 :doc:`development`；本次未重跑物理触发测试。软件覆盖超过 ``2^53`` 的精确时间、uint32 回绕、重复/零码、
同锚点多事件、事件单独到达与拆包、有界队列溢出和 stop/start。

相关设备侧测试摘要、固件身份与复现步骤保存在
`固件验证索引 <https://github.com/Omni-Intel/omnibci-firmware-stm32/blob/a20876d6131ecbf51e4eda595f92f866303d99cf/docs/validation/README.md>`_；
它们与绑定专属测试范围不同，不能互相替代。以上均为已有证据，本次文档整理
未新增硬件验收。
