# Process Job Hacker 实施计划

> Rust + eframe/egui + Windows Job Object 的进程树 CPU/内存限额工具。
>
> 状态标记：`[x]` 已完成并通过运行/测试验证；`[ ]` 未完成或尚未验证。
> 最后更新：2026-11-09（主窗口、托盘、P/E 核图、真实 Job 限额、目标重启恢复、崩溃守护均已验证）

## 0. 环境与骨架

- [x] 升级 Rust stable 到 >= 1.95（当前 rustc 1.99.1）
- [x] 创建 Cargo 工程与目录结构
- [x] 配置依赖：eframe/egui 0.37、windows-sys 0.61、sysinfo 0.40、tray-icon 0.26、muda 0.21、serde、crossbeam-channel 等
- [x] 配置 build.rs、app.manifest、app.admin.manifest
- [x] `cargo check` 通过
- [x] 窗口/exe/托盘图标（assets/icon.png、tray-icon.png、app.ico）
- [ ] release 资源版本信息最终核对

## 1. 应用骨架与中文 UI

- [x] main.rs：日志、panic hook、eframe 启动、单实例入口
- [x] app.rs：PjhApp 状态、Panel 布局、App::logic/ui
- [x] 中文字体加载（微软雅黑/黑体，system_fonts 兜底）
- [x] 深色主题与基础样式
- [x] `cargo run` 实际打开主窗口并手动验证

## 2. 托盘与生命周期

- [x] tray-icon + muda：托盘图标、菜单、左键显示/隐藏
- [x] 关闭窗口 = 隐藏到托盘，不清理限制
- [x] 托盘退出 = 清理限制后退出（确认框代码）
- [x] 单实例命名互斥体 + 第二次启动唤起窗口
- [x] 托盘事件通过 AppCommand + ctx.request_repaint 进入 App::logic
- [x] `cargo check` 通过
- [x] 托盘/隐藏/二次启动唤起实际运行验证
- [ ] 托盘菜单“退出并清除限制”实际点击验证（崩溃守护清理已验证）

## 3. 进程采集与进程树

- [x] sysinfo 后台采集线程（可见 1s / 隐藏 2s；CPU 采样保持 1s）
- [x] PidKey = PID + start_time，防 PID 复用
- [x] ProcessTree 构建、孤儿/父进程已退出处理
- [x] 虚拟化进程树：名称、PID、PPID、CPU%、工作集、提交、规则标记
- [x] 搜索、展开/折叠、刷新暂停、状态栏
- [x] `cargo run` 验证 529 进程、刷新正常
- [x] 名称列左对齐并按层级向右缩进
- [x] PID/PPID/CPU/工作集/提交/规则列与表头右对齐
- [x] 右键菜单“一键限制到 P0（CPU 0-1，持久化）”
- [x] P0 掩码按物理核全部逻辑线程生成，子进程继承
- [x] 一键 P0 始终写入 config.json；已有临时策略先清理旧 Job 再转持久策略

## 4. CPU 拓扑与限额编辑器

- [x] 逻辑 CPU 枚举（GetActiveProcessorCount / GetSystemInfo）
- [x] GetLogicalProcessorInformationEx 解析物理核与 EfficiencyClass
- [x] 自动区分 P 核 / E 核（i7-12700：P8 核 16 线程，E4 核）
- [x] 亲和性网格按物理核分组（P0、E0…），支持全选/清空/反选
- [x] 内存上限输入（MiB/GiB、预设、当前提交对比）
- [x] 高级项：CPU 率硬上限、SUBSET_AFFINITY
- [x] 进程详情面板：路径、启动时间、Job 状态
- [x] `cargo check` 通过

## 4.1 顶部 CPU 核心图

- [x] 后台 1 秒采样所有逻辑核，60 点环形缓冲
- [x] 顶部可折叠 CPU 面板：总 CPU + 每核迷你折线图
- [x] 默认淡蓝；单点 >80% 黄色、>95% 红色
- [x] E 核默认绿色折线，P 核/总 CPU 保持蓝色；阈值色优先
- [x] 折线分段着色、当前值与边框按最新值着色
- [x] 25/50/75/100% 网格、60 秒时间轴、悬停 tooltip
- [x] 隐藏到托盘继续 CPU 采样
- [x] P 核折线图独立分组，E 核另起一行
- [x] 自动识别 P/E 核（EfficiencyClass 高者为 P 核）
- [x] 面板折叠状态持久化（高度暂未单独持久化）
- [ ] CPU 面板高度持久化
- [x] 单元测试：环形缓冲、阈值、核心数变化、P/E 分类
- [x] 实际运行验证 P16 + E4 布局与配色
- [x] 单元测试：P0 双线程掩码为 0b11，不存在时返回 None

## 5. Job Object 核心

- [x] JobHandle：创建/打开命名 Job、RAII 关闭
- [x] set_limits：JOB_OBJECT_LIMIT_JOB_MEMORY + AFFINITY + CPU rate
- [x] clear_all_limits：清零 ExtendedLimit + CPU rate
- [x] 原始亲和性捕获与恢复
- [x] CPU 亲和性改为逐进程 SetProcessAffinityMask（子进程继承，退出可恢复）
- [x] Job 仅负责总提交内存 / CPU 率，避免 Job 销毁后亲和性残留
- [x] Assign：先根进程、后快照后代
- [x] 查询成员、限额、峰值、ActiveProcesses
- [x] 关键进程/本程序自身保护；现有 Job 提示
- [x] 应用失败清单与中文错误映射
- [x] 纯亲和性策略跳过 Job，直接逐进程设置掩码
- [x] 已在其他 Job 中的进程（接收 Job 非空时）归类为仅亲和性，不再逐条报错
- [x] journal 合并原始亲和性，自动重应用不覆盖首次基线
- [x] 恢复集合包含未加入 Job、仅应用亲和性的进程
- [x] 集成测试验证亲和性/内存限额/清理
- [x] 真实进程验证：Job 内存限额 64MiB；CPU 亲和性 0-1 由逐进程设置生效

## 6. 持久策略

- [x] Policy 模型：match(exe_path/name)、limits、enabled、auto_reapply、job_name
- [x] config.json 原子读写 + 版本字段
- [x] 策略页：列表、状态、启用/暂停、编辑、删除、立即应用
- [x] “保存策略并应用” vs “仅本次应用”
- [x] 按 exe_path 匹配最顶层根进程，复用命名 Job
- [x] 目标重启自动重新应用（1.5s 调度、3s 失败退避）
- [x] `cargo test` 通过（22 项）
- [x] 实际运行验证：timeout.exe 策略保存、目标重启后 4s 内重新限制
- [ ] 多实例提示实际验证

## 7. 生命周期：退出清理、启动恢复、崩溃守护

- [x] active_jobs.json 原子日志（Job 名、roots、原始亲和性）
- [x] 正常退出：清限额 → 恢复亲和性 → 关闭 Job → 清空日志
- [x] 启动顺序：等待/终止残留 guard → 清理旧 Job → 启动新 guard → 恢复策略
- [x] guard 模式 `--guard`：等待父进程退出，清理 PJH 前缀 Job
- [x] guard 只清理 Local\PJH- 前缀，防误清
- [x] `--clear-persisted` 崩溃恢复
- [x] `--safe-mode` 只清理、不自动应用
- [x] 退出清理失败时阻止退出并显示原因
- [x] 实际运行验证：强杀主程序后守护进程恢复全核亲和性、清空 journal
- [x] 实际运行验证：主程序重启后策略重新生效（timeout.exe 亲和性恢复为 0-1）
- [x] 无 Job 句柄时仍可按 PID + start_time 从 journal 恢复亲和性
- [x] 纯亲和性策略无 Job 时，退出/崩溃仍可恢复亲和性
- [x] 策略卡片“恢复全核并暂停”修复旧版本污染的原始掩码
- [ ] 实际运行验证：退出后目标可超额分配内存
- [ ] 托盘菜单正常退出路径实际点击验证

## 8. 打磨与发布

- [x] 右键菜单：设为限额目标、一键限制到 P0、打开文件位置、终止进程
- [x] 一键 P0：已有策略更新亲和性并保留内存/CPU 率；无策略创建持久策略并写 config.json
- [x] 详情页复制 PID/路径
- [x] 策略状态色、tooltip、空状态
- [x] 失败清单弹窗、退出确认、终止确认
- [x] README：架构、用法、限制、恢复步骤、界面截图
- [x] release 构建成功（target/release/process-job-hacker.exe，约 16.4 MB）
- [x] exe 图标：winres 嵌入多尺寸 app.ico；窗口/托盘使用裁剪后的 PNG
- [ ] 托盘菜单“退出并清除限制”手动点击验收
- [ ] 退出后目标内存超额分配手动验收

## 9. 最终验证

- [x] `cargo fmt --check`
- [x] `cargo clippy --all-targets -- -D warnings`
- [x] `cargo test`（22 项通过）
- [x] `cargo test -- --ignored`（0 个 ignored，正常）
- [x] release 构建成功
- [x] 手动：关闭到托盘、二次启动唤起、进程树、P/E 图
- [x] 手动：目标重启后自动重新限制
- [x] 手动：强杀主程序后崩溃守护恢复亲和性并清空 journal
- [x] 手动：主程序重启后策略重新生效
- [x] 手动：真实 Job 内存 64MiB；timeout.exe 亲和性 0-1，退出/崩溃后可恢复
- [ ] 手动：托盘菜单“退出并清除限制”点击验收
- [ ] 手动：退出后目标大内存分配超过旧上限
- [ ] 手动：CPU 图 80%/95% 阈值配色（需制造高负载）
- [ ] 手动：右键一键限制到 P0 的菜单点击验收

## 当前状态

核心功能已完成并通过构建、静态检查、单元/集成测试和真实进程验证。
纯亲和性策略不再创建 Job，Doubao 等自带 Job 的进程不会再出现批量 `AssignProcessToJobObject` 报错；
混合策略会把无法嵌套的进程汇总为“仅亲和性”。剩余为托盘正常退出路径、过高内存分配和阈值配色的手工验收。

