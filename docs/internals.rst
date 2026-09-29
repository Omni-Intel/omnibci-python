内部接口索引
============

内部接口不保证兼容性，应用程序使用 :doc:`api`。

Python 辅助接口
---------------

位于 ``python/omnibci/api.py``：

.. list-table::
   :header-rows: 1
   :widths: 55 45

   * - 签名（省略 self / cls）
     - 用途
   * - ``_timeout(value: float) -> float``
     - 校验 timeout 类型、有限性和范围。
   * - ``_combine(batches: list[SampleBatch]) -> SampleBatch | None``
     - 合并同一采集代数和采样率的批次；空列表返回 None。
   * - ``Board.__init__(native_device)``
     - 初始化原生设备引用、待读队列和锁。
   * - ``Board._ensure_open() -> None``
     - 检查 Board 未关闭。
   * - ``FrontendConfig.__post_init__() -> None``
     - 校验字段，规范化 gains。
   * - ``FrontendConfig._wire() -> dict``
     - 转成原生配置格式，不包含 verified。
   * - ``FrontendConfig._from_wire(value: dict) -> FrontendConfig``
     - 类方法，从原生配置构造数据类。
   * - ``SampleBatch._slice(start: int, stop: int | None = None) -> SampleBatch``
     - 切分样本并保留元信息。
   * - ``SampleBatch._from_wire(data: dict) -> SampleBatch``
     - 类方法，将原生 bytes 缓冲区转换为 NumPy 数组。
   * - ``PartialReadError.__init__(partial: SampleBatch | None)``
     - 保存部分结果并生成异常消息。

原生扩展
--------

``omnibci._native`` 由 ``src/lib.rs`` 实现，类型声明位于
``python/omnibci/_native.pyi``。该层读取返回缓冲区字典，公开 Python 层负责
转换成 SampleBatch，并实现固定长度读取和剩余样本缓存。

.. code-block:: python

   class NativeDevice:
       @staticmethod
       def connect(endpoint: str, timeout: float) -> NativeDevice: ...
       def start(self, timeout: float) -> None: ...
       def stop(self, timeout: float) -> None: ...
       def close(self, timeout: float) -> None: ...
       def configure(self, config_json: str, timeout: float) -> None: ...
       def snapshot_json(self) -> str: ...
       def read(self, timeout: float) -> dict[str, Any]: ...

   def serial_ports() -> list[str]: ...
   def discover_ble_json(timeout: float) -> str: ...
   def decode_frames(data: bytes, gains: tuple[int, ...]) -> dict[str, Any]: ...

除 ``PartialReadError`` 在 Python 层定义外，公开异常均由原生扩展定义并重新导出。
原生扩展同时提供 ``SDK_VERSION`` 和 ``SDK_REVISION``。
