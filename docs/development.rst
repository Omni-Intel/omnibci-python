构建与维护文档
==============

构建 HTML
---------

需要 Python 3.11+ 和 Sphinx，无需安装 omnibci。在仓库根目录执行：

.. code-block:: console

   python -m venv .venv-docs

Windows PowerShell：

.. code-block:: powershell

   .\.venv-docs\Scripts\python.exe -m pip install -r docs/requirements.txt
   .\.venv-docs\Scripts\python.exe -m sphinx -n -W --keep-going -b html docs docs/_build/html
   Start-Process docs/_build/html/index.html

Linux / macOS：

.. code-block:: console

   .venv-docs/bin/python -m pip install -r docs/requirements.txt
   .venv-docs/bin/python -m sphinx -n -W --keep-going -b html docs docs/_build/html

生成入口为 ``docs/_build/html/index.html``。上述命令将缺失引用和其他警告视为失败。

维护约定
--------

``conf.py`` 从 ``pyproject.toml`` 读取版本。导航在 ``index.rst`` 中维护。

API 签名在 ``api.rst`` 中手写。接口变更时，对照 ``python/omnibci/__init__.py``
的导出、``python/omnibci/api.py`` 和 ``python/omnibci/_native.pyi`` 更新文档。
Sphinx 仅检查语法和交叉引用，不核对源码或执行采集示例。
