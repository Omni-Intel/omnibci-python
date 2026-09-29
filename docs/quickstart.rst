发现与采集
==========

USB 串口
--------

关闭占用设备的程序，将 ``COM5`` 替换为设备端口。
串口发现只枚举候选端口，不验证设备身份。

.. code-block:: python

   from omnibci import Board

   for device in Board.discover():
       print(device.name, device.endpoint)

   with Board.connect("serial://COM5") as board:
       print(board.info)
       print(board.get_config())
       board.start()
       batch = board.read(250, timeout=3)
       board.stop()
       print(batch.eeg_uv.shape)  # (250, 8)，微伏
       print(batch.sample_time_s)
       print(board.snapshot)

Linux endpoint 示例为 ``serial:///dev/ttyACM0``，macOS 示例为
``serial:///dev/cu.usbmodem...``。

BLE
---

.. code-block:: python

   from omnibci import Board

   devices = Board.discover(transport="ble", timeout=10)
   for index, device in enumerate(devices):
       print(index, device.name, device.endpoint, device.rssi_dbm)
   if not devices:
       raise RuntimeError("未发现 BLE 设备")

   # 多台设备时，选择目标设备的索引。
   with Board.connect(devices[0], timeout=20) as board:
       board.start()
       for _ in range(10):
           batch = board.read(250, timeout=5)
           print(batch.eeg_uv.mean(axis=0))
       board.stop()

BLE key 是平台相关标识，不能用 MAC 地址代替。
连接时使用发现结果中的 ``DeviceInfo`` 或 ``endpoint``。

连续批次
--------

.. code-block:: python

   from omnibci import Board

   with Board.connect("serial://COM5") as board:
       board.start()
       for index, batch in enumerate(board.iter_batches(250, timeout=3)):
           print(index, len(batch), batch.eeg_uv.mean(axis=0))
           if index == 9:
               break
       board.stop()

``iter_batches()`` 的读取异常向外传播，需显式开始和停止采集。
用 ``with`` 或 ``close()`` 释放设备；``with`` 在异常退出时也会关闭设备。

保存数据
--------

.. code-block:: python

   import numpy as np
   from omnibci import Board

   with Board.connect("serial://COM5") as board:
       board.start()
       batch = board.read(2500, timeout=15)
       board.stop()

   np.savez(
       "eeg.npz",
       eeg_uv=batch.eeg_uv,
       raw_counts=batch.raw_counts,
       valid=batch.valid,
       sequence=batch.sequence,
       sample_indices=batch.sample_indices,
       sample_rate_hz=batch.sample_rate_hz,
       generation=batch.generation,
   )

``sample_indices`` 和 ``valid`` 分别保留丢包间隔和样本有效标志。
