# OwlBTLock 功能调试记录

状态：[OPEN]（2026-11-11 已完成静态复查，运行时验证仍待真机执行；详见 FUNCTIONAL_REVIEW_2026-11-11.md）
日期：2026-09-08
平台目标：Ubuntu 24.04 / GNOME / Wayland

## 用户可见问题

- 窗口曾出现黑屏、白屏和重复启动窗口。
- 设备列表、按钮和页面布局多次异常。
- 手机或蓝牙设备已连接，但应用主页面仍显示未连接。
- 需要确认重写后原程序功能没有丢失，并完成逐项测试和日志核对。

## 可证伪假设

1. H1：黑屏/白屏由 GTK4 CSS 选择器、主题变量或 GSK/GDK 渲染后端兼容导致。
2. H2：设备连接状态错误来自蓝牙命令输出解析错误或扫描得到的 MAC 与监控使用的 MAC 不一致。
3. H3：后台轮询已获得新状态，但 GTK 主线程没有执行对应 UI 更新。
4. H4：按钮未靠底是 GTK 容器没有正确分配剩余垂直空间或底部对齐。
5. H5：锁屏、解锁、配置、自启动和状态机等模块存在未被运行验证的回归。

## 调试约束

- 先收集运行时证据，再做业务逻辑修复。
- 首次代码变更只能增加诊断日志。
- 保留本记录直到用户确认功能修复。

## 验证清单（2026-11-11 静态复查结论）

标记说明：`[x]` 静态通过且无需运行时证据；`[!]` 代码有缺陷；`[ ]` 仍需真机端到端验证。
本环境无 session/system D-Bus、无蓝牙服务、无显示后端，凡涉及真实蓝牙/GUI/锁屏的项均不能勾为完成。

- [x] 编译检查 — `cargo check` 0 error / 15 warnings；`cargo test` 4 passed / 0 failed
- [!] GTK 启动与单实例检查 — `connect_activate` 未判重，二次启动会重复建窗（main.rs:315）
- [ ] 设备扫描和列表渲染 — 代码链路完整，本机无蓝牙服务，未实测
- [x] 设备选择 — `select_device_from_pick` 写配置、切页、刷新标签、重启监控
- [x] 已连接/断开状态轮询 — BlueZ GetAll + 去抖 + bluetoothctl 兜底；`find_device_om` 路径提取有隐患
- [!] RSSI 或无 RSSI 降级逻辑 — 降级正确，但低信号误发 Disconnected 并触发重连取消倒计时
- [ ] 锁屏触发 — 链路完整，两处失效点（P0-2 / P0-3），未实测
- [ ] 解锁/恢复连接 — 有 4 路实现，锁屏后能否免密解锁未验证
- [x] 手动开始/停止 — `on_toggle_monitor` 状态分支完整，重启监控前会停旧线程
- [x] 更换设备 — 按钮回调 → 停监控 → 切页 → 重扫描
- [x] 配置保存和重启加载 — 2026-11-12 已修复：唯一用户级配置源 + 原子写入 + 4 个回归测试；用户选中的设备不再被另一份配置覆盖
- [!] 自启动安装/卸载逻辑 — unit 缺 Wayland 环境变量；UI 同步会误触发 enable/disable
- [ ] Wayland运行路径 — 已有 cairo/Adwaita 适配，未实测
- [ ] X11兼容运行路径 — 有 xprintidle/xssstate 回退（本机未安装），未实测

## 证据记录

### 2026-11-11 静态复查

- 构建：`cargo test` → `test result: ok. 4 passed; 0 failed`；`cargo check` → 15 warnings（多为 dead_code）。
- 配置分裂实证：`diff -u .config/config.json ~/.config/owlbtlock/config.json` 显示
  `phone_mac` 分别为 `8C:7A:3D:2D:26:FF`(Redmi K40) 与 `D3:4A:1E:E1:39:A4`(Logi POP Mouse)。
- 环境探测：`gdbus call --session` / `--system` / `bluetoothctl devices Paired` 均报「无法连接」；
  `systemctl is-active bluetooth` 报「System has not been booted with systemd」；`Xvfb`/`xvfb-run`/`xprintidle` 均 MISSING。
  因此本轮无运行日志与蓝牙命令输出可采集。
- 完整结论、问题分级与代码行号见 `FUNCTIONAL_REVIEW_2026-11-11.md`。

### 待补充（需真机）

真机 Ubuntu 24.04 / GNOME Wayland + 已配对手机，逐项采集：窗口启动与单实例、扫描列表、
断开/重连状态流转、RSSI 远离锁屏、锁屏与解锁、倒计时期间用户活动、开机自启、X11 会话。
