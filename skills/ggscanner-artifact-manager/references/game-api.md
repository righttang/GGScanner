# 本机 API、按键与回执

默认 `http://127.0.0.1:18765`，端口以配置为准。服务仅绑定 loopback；WSL 到
Windows 优先用 `curl.exe`。完整规范见项目 `docs/MANAGER_API.md`，独立安装
Skill 后可读 [在线规范](https://github.com/righttang/GGScanner/blob/main/docs/MANAGER_API.md)。

## 串行任务

- `GET /health`、`GET /status`：启用、忙碌、游戏存活与分项进度。
- `POST /scan`：如 `{"characters":true,"weapons":true,"artifacts":true}`，返回 jobId。
- `POST /equip`、`POST /manage`：完整 GOOD 身份；外形先查权威规范。
- `GET /result?jobId=...`：该任务最终结果。
- `GET /characters?jobId=...`、`/weapons?jobId=...`、`/artifacts?jobId=...`：扫描产物。
  分类未完成返回503，不能当成空库存。
- `POST /stop`：`{"jobId":"当前ID"}`，随后等待终态。

202仅表示已接受。409可能是忙碌或 ID 冲突，先读状态与回执。不同时运行其他
鼠标脚本，不用任意另一个任务的结果充当本次证据。

## 点击与按键

`POST /input/click` 使用游戏客户区实际像素，非桌面坐标，也不会自动按1080p缩放。
OCR区域亦须在窗口内，一行一个小区域。

```json
{"requestId":"inspect-one","x":1740,"y":1018,"delayMs":800,
 "ocr":[{"name":"result","x":1200,"y":130,"width":250,"height":50}]}
```

Windows 的独立 `POST /input/key` 不需要 x/y：

```json
{"requestId":"walk-one","key":"w","holdMs":180,"delayMs":500,"ocr":[]}
```

`holdMs` 为0–3000，0为点按；`delayMs` 为0–5000，是松键/点击后等待再OCR。
键盘请求不能混入鼠标或文本字段。最多16个OCR区域。

键名：`w/a/s/d`（或 `forward/left/backward/right`）、`space/jump`、`shift/sprint`、
`e/skill`、`q/burst`、`1`–`4`（或 `party1`–`party4`），以及 `menu`（Esc）、
`partySetup`（L）、`inventory`（B）、`character`（C）、`map`（M）、`interact`（F）、
`confirm`（Enter）。只接受固定游戏动作，没有任意系统快捷键或持久按下接口。

一次一个键，从短按试走。传送后先确认目的地、加载完成与交互提示，再靠近并按F。
失去焦点、右键中断或 `/stop` 结束长按并尝试松键；正常返回、出错及异常展开也会
松键。进程被强制结束或系统故障不属于正常回执保证。

`/input/key` 与长按请求立即返回202，期间可查询或停止。取
`GET /input/result?jobId=请求ID` 至 `state=completed`，检查 feedback 与各OCR错误。
`clicked=true`只表示输入已提交，不证明打造/强化成功；`null`表示可能有部分输入，
先观察现场。Windows 文本替换用点击的 `text`，1–64个可打印字符，不能与key共用。

## 落盘与恢复

提交前生成唯一 requestId 并保存完整请求。同 ID 同内容去重，不同内容409。
仅当前进程最近64项回执可用。服务重启或过期后先观察，不能依赖旧 ID 重发。
扫描/穿戴/锁定没有去重保证，结果未知时先查状态和现场。

附带客户端遇网络异常不重发 POST；已有输入台账再次执行只查询原回执，404后
需要人工判断现场。默认台账目录含用户数据，不提交到公共仓库。
