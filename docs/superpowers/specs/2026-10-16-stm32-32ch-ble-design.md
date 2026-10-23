# Python wheel：32 导 E73 BLE 适配方案

> 历史设计记录：本文保留当时的工作分支、SDK 基线与讨论，当前安装和设备能力见 [README](../../../README.md)，接口契约见 [API 参考](../../api.rst)。

## 目标与工作分支

在 omnibci-python 的 `32ch` 分支增加 STM32 32 导支持，优先支持用户现有 GUI/firmware 的 E73 BLE 直连版本。沿用 omnibci 包名、Python >=3.11 和 ABI3 wheel；保持现有 ESP32 8 导 API 兼容。沿用仓库六平台 wheel 构建范围，本机先验证 Windows x64。用户最后一句 wheel 要求未完整发出，如补充要求，以其为准。

## 已核对的基线

- Python 起点：93158d7c9dd8a1ff4bacc78e4cf9a69b87c67352，远端已存在 32ch 分支。
- 本地 GUI：fix/e73-wireless-recovery，13d6342，工作区干净。
- GUI 实际引用的 SDK：cc76914882d2b84bc5c14780b136cd82e5083465，工作区干净。
- 本地 firmware：fix/e73-wireless-recovery，4a6619e，工作区干净。
- 2026-10-16 查询上述三个仓库的远端同名开发分支均未返回引用。这些提交不可假定已发布；最终发布前须保证固定 SDK 提交可从远端获取，不能交付仅本机可构建的子模块引用。
- 固件记录包含 BLE ATT ACK 超时，最新恢复候选尚未完成实机门槛。适配不能宣称解决这些故障或通过硬件稳定性验收。

## 方案选择

推荐复用 GUI 所用的 Rust E73/STM32 驱动，在 Python 原生层增加最小同步会话适配。复制 GUI 协议代码会产生两套实现；另建一个 Python 包则重复发布和维护，均不采用。

## 对外行为

1. Board.connect/discover 增加显式设备选择 `device_type="stm32"`，默认仍为 ESP32。STM32 下 BLE 使用 E73 专用发现和驱动；串口使用相同的 STM32 V3 驱动，支持板端 USB 与 ESB dongle。
2. 新增独立 Stm32Config，映射 SDK 的 mode、enabled、bias、loff、32 个 gains。保留原 FrontendConfig 的八导语义，避免把 ESP32 的 SRB2 配置错误应用到 STM32。默认 loff=0；配置仅允许停止状态，确认值以硬件回执为准。
3. 复用 Board 的 start/stop/read/iter_batches/close/context manager。STM32 数据返回 (N,32)，250 SPS；各板型的状态字节宽度依据协议，校验 buffer 形状，不补造数据或状态。保留序号缺口和原始计数，逐通道按已确认增益换算。
4. 提供完整 V3 缓冲区的离线解码入口，并明确设备类型；保留原 decode_frames 默认八导行为。硬件事件与原始状态只暴露 SDK 实际提供的信息。

## 传输、生命周期与错误

- 复用 e73_ble 的 2M TX/RX、MTU>=153、真实 SPI 桥接固件检查、重排、ACK/NACK 和 CRC 实现，不在 Python 重新实现无线协议。
- 配置和 START 等待 STM32 执行回执。STOP 等待停止及 SD 关闭回执，先交付已接收尾帧；失败不可宣称已排空或硬件已停止。
- 原生会话管理有界队列、截止时间和 worker 生命周期；底层事件回调不阻塞，不静默丢弃溢出数据。关闭及错误清理必须有界，保留原始错误和已消费的部分样本。
- BLE 断开终止会话，不自动重连或隐式切换无线模式。第一版只连接当前模式；GUI 的 Flash 模式切换流程和阻抗数值计算不纳入本次 Python 适配。LOFF 配置退出时须确认关闭激励，失败需明确报告。
- 完整配置要求支持 AE/BE 事务的 32.0.5 系列固件；32.0.4 不走此路径，不修改其验证发行内容。

## 验证与交付

- 先添加失败测试，再实现：真实 V3 帧解析、32 位掩码和 32 个增益、形状、CRC/分包/序号缺口，以及配置/启停确认、超时、尾包、断开和溢出。保持现有 ESP32 回归通过。
- 固定 SDK 源提交和 Cargo.lock；当前 Python 仓库无 uv.lock，补充并提交本仓库锁文件后使用 uv sync --locked --all-extras，再以 uv run --no-sync 执行 Python 工具。不得使用其他工程虚拟环境。
- 构建 Windows x64 ABI3 wheel，在独立 uv 环境安装该 wheel 验证导入、API 与离线 V3 解码，核对版本和 SHA256。更新说明、类型声明和 BLE 示例。
- 跨平台远端构建通过后才能声明相应平台 wheel 可用；发布前核对源提交可获取、版本/产物哈希与 GitHub Actions 状态。软件测试与实机验证分开记录。

## 自审

基线与用户指定的 GUI/firmware 一致；BLE 不是 ESP32 服务，也不是 dongle 串口替代名。没有将未发布提交或开发候选误写为稳定发行。第一版范围为当前模式下的连接、配置和数据采集，模式切换保持显式不支持。
