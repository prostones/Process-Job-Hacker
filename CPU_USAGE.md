# CPU 占用调查记录

- 日期：2026-11-12
- 状态：已知问题，暂不修复
- 运行环境：release 安装版（管理员权限），i7-12700，16 核 24 线程

## 现象

- 程序空闲、窗口可见时，`process-job-hacker.exe` 在任务管理器里 CPU 占用约 **16%**。
- 点击工具栏「暂停刷新」后，进程 CPU **直接降到 0%**。
- 关闭窗口隐藏到托盘后 CPU 也会明显下降。

## 已排除

**不是 CPU 折线图的问题。**

暂停刷新时，`src/monitor.rs` 仍然每秒执行：

- `system.refresh_cpu_usage()`
- `cpu_history.push(...)`
- `ctx.request_repaint()`

CPU 面板和 20 个核心折线图仍然每秒重绘，但进程 CPU 已经是 0，说明图表绘制和 `request_repaint` 的开销接近 0。

## 开销构成（按嫌疑排序）

| 环节 | 位置 | 说明 |
| --- | --- | --- |
| `sysinfo` 全量刷新进程 | `src/monitor.rs` `refresh_processes_specifics(ProcessesToUpdate::All, true, process_kind)` | 每秒枚举并刷新 500+ 进程的 CPU/内存，最大嫌疑 |
| `build_snapshot` | `src/monitor.rs` `build_snapshot` | 每秒为 500+ 进程重新分配 `name: String` 和 `path: PathBuf` |
| `ProcessTree::build_with_sort` | `src/model/tree.rs` | 每秒建树 + 排序；`ProcessSort::compare` 每次比较都调用两次 `to_lowercase()`，产生大量临时 String |
| `refresh_runtimes` | `src/app.rs` | 每秒对 17 个策略执行 `query_members` / `query_limits` 等 Win32 调用，并遍历成员 |
| `auto_apply_new_matches` | `src/app.rs` | 每 1.5 秒 × 17 个策略 × 500+ 进程做匹配 |

`AppConfig::refresh_visible_ms` / `refresh_hidden_ms` 已经持久化，但 `monitor.rs` 目前硬编码 1 秒 / 2 秒，没有使用配置。

## 优化建议

按收益排序，建议分步做并每步测量：

1. **进程刷新降到 2 秒**
   - `src/monitor.rs` 的 `process_due` 增加 `tick.is_multiple_of(2)`。
   - CPU 刷新保持 1 秒，CPU 折线图不变。
   - 预期：16% → 8% 左右。

2. **排序预计算小写名称**
   - `src/model/tree.rs` 在 `build_with_sort` 前为每个 `ProcessRecord` 计算一次小写名，`ProcessSort::compare` 直接比较。
   - 预期：再降 1–2%。

3. **`auto_apply_new_matches` 降频**
   - `src/app.rs` 的 1.5 秒间隔改为 3 秒，或只在 PID 集合变化时执行。
   - 预期：再降 1–2%。

4. **`build_snapshot` 复用上一帧记录**
   - 按 `PidKey` 复用 `ProcessRecord` 的 `name` / `path`，只更新 `cpu_percent` / `memory` / `commit` / `parent_pid`。
   - 预期：再降 1%。

5. **可选：`sysinfo` 增量刷新**
   - `ProcessesToUpdate::Some(&known_pids)` 只刷新已知进程，每 5 次全量发现新进程。
   - 风险：新进程最多延迟约 10 秒出现，需要确认是否接受。

## 验收目标

- release 空闲（窗口可见、未暂停）：**< 5%**
- 暂停刷新：**< 1%**
- 隐藏到托盘：**< 1%**

## 待确认

1. **进程列表刷新间隔**：默认改成 2 秒；如果觉得更新太慢，可以只降 `sysinfo` 频率、界面保留 1 秒。
2. **新进程延迟**：默认先不做增量刷新，靠 2 秒全量；如果 2 秒还不够再讨论。
3. **目标 CPU**：默认 release 空闲 **< 5%**。

## 复现与测量方法

1. 启动 release 安装版，保持窗口可见，等待 10 秒。
2. 任务管理器 → 详细信息 → 找到 `process-job-hacker.exe`，记录 CPU。
3. 点击「暂停刷新」，记录 CPU。
4. 关闭窗口到托盘，记录 CPU。
5. 优化后重复上述步骤对比。
