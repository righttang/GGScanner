# Windows 本机控制 / Windows local control

本修改版基于 [Anyrainel/GGScanner](https://github.com/Anyrainel/GGScanner)。
保留原有两个 Windows 版本与开源致谢；本仓库中的程序只从
[righttang/GGScanner](https://github.com/righttang/GGScanner/releases) 检查更新。

## 启动与 HDR

Windows 程序携带管理员权限清单。正常启动时允许 UAC 授权；原神页面会自动启动
配置端口的本机服务，默认 `127.0.0.1:18765`。不必另外启动一次管理器。
HDR 设置应与 Windows 的“使用 HDR”状态一致。捕获只接受请求后产生的新帧，避免
扫描角色切换后仍识别上一帧；HDR 下也能识别祝圣之霜定义的圣遗物。
无法识别的等级会明确报错或跳过，不会当作 `+0` 导出。

## 一次授权后的启停

这是可选功能，只在用户主动运行安装命令后注册。请从要保留的程序目录运行：

```powershell
.\GGScannerOCR.exe --install-local-launcher
```

授权后生成 `GGScannerOCR.lnk` 和 `data/local-launcher.json`。快捷方式启动同一份
程序。任务仅在当前用户登录时按需运行，没有自动触发器，不保存密码，也不提供
执行其他程序或命令的入口。后续可以用以下命令启动和停止：

```powershell
$launcher = Get-Content .\data\local-launcher.json -Raw | ConvertFrom-Json
Start-ScheduledTask -TaskName $launcher.taskName
Stop-ScheduledTask -TaskName $launcher.taskName
```

停止前先检查 `/health`，任务进行中通过 `/stop` 请求停止并等待完成。若需要安装
另一份本地构建，可将同名暂存程序传给安装命令的目标参数；文件占用时使用已有
更新器的暂存、重试及回滚流程，避免直接覆盖运行中的文件。

## 服务与操作

`GET /health`、`GET /status` 在扫描期间仍保持监听；`POST /stop` 携带当前 `jobId`
请求停止。界面扫描与 HTTP 扫描、穿戴、锁定、输入共用执行队列，不会同时操作游戏。
开始下一项操作前等待当前任务结束。完整格式见 [API 文档](MANAGER_API.md)。

`POST /input/click` 支持游戏客户区像素坐标、`delayMs`（0–5000 毫秒）和最多 16 个
OCR 区域。输入会校验窗口、边界与焦点。`requestId` 用于去重；超时后查询
`GET /input/result?jobId=...`，不要换 ID 重复提交。回执仅在服务进程内保留最近 64 项。

可选 `key` 只接受固定游戏动作：`menu`、`partySetup`、`inventory`、`character`、
`map`、`interact`、`confirm`。Windows 可用 `text` 在指定游戏输入框中替换 1–64 个
可打印字符。`key` 与 `text` 不能同时提供。游戏自身的队名长度限制仍然适用。
`/input/*` 仅接受无 Origin 的本机客户端或 loopback Origin；服务不绑定外网地址。

## English

Windows builds request administrator privileges and automatically start the Genshin
loopback service on the configured port, normally `127.0.0.1:18765`. Match the HDR
setting to Windows. Fresh-frame capture and HDR Artifact banner recognition fixes
are shared by scanning and game-input feedback.

The optional `--install-local-launcher` command registers an explicitly authorized,
on-demand task for this installed executable and the current interactive user. It
creates a shortcut and `data/local-launcher.json`; there are no automatic triggers
or stored passwords. Stop active jobs through `/stop` before stopping the process.

GUI scans and HTTP jobs share one execution queue while health, status and stop
requests remain available. Game input supports bounded coordinates, delayed OCR,
fixed navigation keys and Windows text replacement. Reuse a request ID to avoid
duplicate input; query its receipt after a timeout. See the [API contract](MANAGER_API.md)
for request examples and completion semantics.

The launcher and HDR support are Windows-only. The experimental Linux scanner
retains its existing platform limitations; see [LINUX.md](LINUX.md).
