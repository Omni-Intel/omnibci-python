数据、超时与错误
================

样本布局与时间轴
----------------

:py:class:`omnibci.SampleBatch` 的数组以样本为第一维。
SDK 返回的数组只读；需要原地处理时使用 ``batch.eeg_uv.copy()``。
``valid`` 标志不会自动过滤数据，按需使用 ``batch.eeg_uv[batch.valid]``。

``sample_indices`` 保留本次采集的丢包间隔，SDK 不补零。
``sample_time_s`` 等于 ``sample_indices / sample_rate_hz``。
``received_at`` 使用主机 ``time.monotonic()`` 时间轴，表示近似 SDK 交付时间；
合批时取最后一批的时间，不代表硬件采样时刻或 Unix 时间。
``generation`` 用于区分成功开始的采集会话。

读取与缓存
----------

* ``read()`` 返回一批可用数据；尚无数据时会等待，受 timeout 限制。
* ``read(n)`` 返回恰好 n 个收到的样本，丢包位置不计入数量。
  n 必须是 1–1,000,000 的整数，多出的样本保留给下一次读取。
* ``stop()`` 保留已接收而尚未读取的尾部样本；新的 ``start()`` 清空上次采集缓存。
* Rust 接收队列最多容纳 128 批。消费过慢导致溢出时报告故障，不静默覆盖旧数据。

超时与部分结果
--------------

所有 timeout 以秒为单位，必须是有限的 int 或 float，范围为 ``(0, 60]``，
不接受布尔值。一次固定长度读取共用一个超时预算。

.. code-block:: python

   from omnibci import Board, OmniBCIError, PartialReadError

   with Board.connect("serial://COM5") as board:
       board.start()
       try:
           batch = board.read(250, timeout=2)
       except PartialReadError as error:
           partial = error.partial
           if partial is not None:
               print("超时前收到的样本：", len(partial))
       except OmniBCIError as error:
           partial = getattr(error, "partial", None)
           if partial is not None:
               print("故障前收到的样本：", len(partial))
           raise
       else:
           print("完整批次：", len(batch))

读取异常的 ``partial`` 保存已消费的样本，不会回填读取缓存；无样本时为 ``None``。
Python 参数错误使用 ``ValueError`` 或 ``TypeError``。异常类型见 :doc:`api`。

线程与异步程序
--------------

同一个 Board 的操作串行执行。读取预算不包括等待其他调用释放 Board 锁的时间。
原生硬件等待释放 GIL。
停止和关闭会等待该 Board 上正在进行的读取完成或超时。

当前 API 是同步阻塞接口。异步程序可用 ``asyncio.to_thread`` 调用，
但取消 Python task 不会中断已经开始的原生操作。

离线解码
--------

.. code-block:: python

   from pathlib import Path
   from omnibci import decode_frames

   raw = Path("capture.bin").read_bytes()
   batch = decode_frames(raw, gains=(24,) * 8)
   print(batch.eeg_uv.shape)

输入必须是原始串口协议字节流，增益应与采集时的实际配置一致。
函数跳过 CRC 错误帧，丢弃末尾不完整帧；每次调用使用独立的解析器。
离线批次的 ``generation`` 为 0。
