前端配置
========

:py:class:`omnibci.FrontendConfig` 是不可变数据类，用 ``dataclasses.replace()``
创建修改后的配置。在停止采集时调用 ``configure()``，等待 SDK 确认。

.. code-block:: python

   from dataclasses import replace
   from omnibci import Board

   with Board.connect("serial://COM5") as board:
       config = replace(board.get_config(), gains=(12,) * 8)
       board.configure(config, timeout=3)
       confirmed = board.get_config()
       print(confirmed)
       assert confirmed.verified

字段与默认值
------------

.. list-table::
   :header-rows: 1
   :widths: 20 20 60

   * - 字段
     - 默认值
     - 含义
   * - ``reference``
     - ``"srb1"``
     - 参考模式：``srb1`` 或 ``srb2``。
   * - ``mode``
     - ``"eeg"``
     - 支持 ``both_bias``、``eeg``、``no_bias``、``shorted``、``test``。
   * - ``enabled_mask``
     - ``255``
     - 通道使能位掩码，bit 0 对应第 1 通道。
   * - ``bias_mask``
     - ``255``
     - BIAS 通道选择，必须是已启用通道的子集。
   * - ``srb2_mask``
     - ``0``
     - SRB2 通道选择，必须是已启用通道的子集。
   * - ``gains``
     - ``(24,) * 8``
     - 8 个通道增益，各自允许 1、2、4、6、8、12、24。
   * - ``verified``
     - ``False``
     - SDK 返回的硬件配置确认标志；发送配置时忽略。

三个掩码必须是 0–255 的整数，不能使用布尔值代替。
SRB1 模式下可以保留非零 ``srb2_mask``，实际 SRB2 开关由参考模式控制。
构造时将 ``gains`` 转为元组并校验。

只启用前四个通道：

.. code-block:: python

   from omnibci import FrontendConfig

   config = FrontendConfig(
       enabled_mask=0x0F,
       bias_mask=0x0F,
       srb2_mask=0,
       gains=(24,) * 8,
   )

读取数据仍使用 8 通道数组布局。配置接口不提供采样率调整，当前采样率为 250 Hz。
