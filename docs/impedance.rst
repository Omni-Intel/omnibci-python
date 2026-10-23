阻抗测量
========

:py:meth:`omnibci.Board.measure_impedance` 在 ESP32 + ADS1299 上执行一次
8 通道电极阻抗测量，返回 :py:class:`omnibci.ImpedanceResult`。测量事务在
Board 锁内完成：保存当前配置、停止采集、关闭 BIAS 反馈、通过原生 SDK 确认
31.25 Hz LOFF 激励、读取指定时长（默认四秒）窗口、停止采集、确认关闭 LOFF，再恢复并校验原配置。
测量结束后板子保持停止状态；需要继续 EEG 采集时必须显式调用 ``start()``。

基本用法
--------

.. code-block:: python

   from omnibci import Board

   with Board.connect("serial://COM5") as board:
       result = board.measure_impedance()  # 默认测量全部已启用通道
       print(f"mask=0x{result.mask:02X} series={result.series_kohm:.2f} kOhm")
       for channel, value in enumerate(result.values_kohm, start=1):
           print(channel, "无效" if value is None else f"{value:.2f} kOhm")
       if result.clipped_mask:
           print(f"削顶通道掩码=0x{result.clipped_mask:02X}")

       # 测量后采集保持停止；不要假定 EEG 会自动恢复。
       board.start()

只测量部分通道时传入位掩码。掩码会与已启用通道求交集：

.. code-block:: python

   result = board.measure_impedance(mask=0x0F, timeout=20)

参数
----

``mask``
   8 位通道掩码，bit 0 对应第 1 通道；默认使用全部已启用通道。
   与 ``enabled_mask`` 求交集后不能为空。

``series_kohm``
   串联电阻补偿值。默认 SRB1 为 9.98 kOhm，SRB2 为 4.40 kOhm。
   显式传值时使用给定值。

``timeout``
   读取指定时长（默认四秒）窗口的总超时，范围 ``(0, 60]`` 秒。LOFF 启停和配置恢复使用
   固定的控制超时。

结果
----

:py:class:`omnibci.ImpedanceResult` 字段：

``mask``
   本次实际测量的通道掩码，已经与已启用通道求交集。

``series_kohm``
   本次使用的串联补偿值。

``values_kohm``
   长度固定为 8 的元组，单位 kOhm。未选中、全量程削顶或有限有效样本
   不足一秒的通道为 ``None``。

``clipped_mask``
   窗口内出现 ADS1299 全量程码的通道掩码，只包含本次 ``mask`` 内的通道。

算法
----

阻抗按 GUI 的 31.25 Hz 正弦/余弦/直流最小二乘拟合计算。ADS1299 交流
LOFF 以 ±6 nA 交替激励，基波峰值为 ``4 * 6 / pi`` nA；拟合幅值除以该
电流后扣除串联电阻。相位使用设备序号，丢帧留下间隔而不会压缩载波。
窗口取最近指定时长（默认四秒）的样本序号区间；任一有效样本达到正负全量程时该通道判为削顶。
每个通道至少需要一秒的有限有效样本。

这些数值是按标称激励电流计算的阻抗估计，与 GUI 一致；电流容差、ADC 响应和
电极特性会影响结果。本次仅完成离线算法及故障恢复验证，尚未进行已知电阻的
实机标定。

失败与恢复
----------

测量前要求配置已确认，且模式为 ``both_bias``、``eeg`` 或 ``no_bias``。
事务失败时 Board 会尽力停止采集、关闭 LOFF 并恢复原配置：

* 清理过程没有其他问题时，原异常保留且不附加 note。
* LOFF 关闭或原配置恢复未确认时，异常附加说明，并关闭该 Board 会话，
  避免在 LOFF 状态不确定时继续 EEG 采集。
* 失败路径不会自动重新开始 EEG 采集。

设备交付的数据超过两秒未送达时会被丢弃并重新累计窗口，避免把过期样本
当作当前阻抗。

可调阻抗窗口
------------

``window_seconds`` 指定采样窗口时长，默认 4 秒，范围为 ``[1, 60]``；
支持小数，按 ``ceil(采样率 * window_seconds)`` 向上取整到采样点。
它不会先等待稳定再采样，缺失样本仍占时间位置；每通道仍须至少一秒有效数据。
``timeout`` 是独立的数据采集超时（最大 60 秒），不会随窗口自动增加；
请为窗口和数据传输预留余量，设置过短可能超时并触发正常清理恢复。

.. code-block:: python

   result = board.measure_impedance(window_seconds=8.0, timeout=15.0)
