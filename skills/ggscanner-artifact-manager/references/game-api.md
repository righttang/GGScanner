# 本机 API、按键与回执

默认 `http://127.0.0.1:18765`，端口以配置为准。服务仅绑定 loopback；WSL 到
Windows 优先用 `curl.exe`。完整规范见项目 `docs/MANAGER_API.md`，独立安装
Skill 后可读 [在线规范](https://github.com/righttang/GGScanner/blob/main/docs/MANAGER_API.md)。

## 串行任务

- `GET /health`、`GET /status`：启用、忙碌、游戏存活与分项进度。
- `POST /scan`：如 `{"characters":true,"weapons":true,"artifacts":true}`，返回 jobId。
- `POST /equip`、`POST /manage`：使用完整 GOOD 身份；请求格式先查接口规范。
- `GET /result?jobId=...`：该任务最终结果。
- `GET /characters?jobId=...`、`/weapons?jobId=...`、`/artifacts?jobId=...`：扫描产物。
  分类未完成返回503，不能当成空库存。
- `POST /stop`：`{"jobId":"当前ID"}`，随后等待终态。

202仅表示已接受。409可能是忙碌或 ID 冲突，先读状态与回执。不同时运行其他
鼠标脚本，不用任意另一个任务的结果充当本次证据。

## 挖矿

由 Scanner 在本机执行。支持 Windows 的原生矿物路线控制；包含地面、滑翔、攀爬、水下、分层地图及固定队伍的已适配动作，逐路线核验实机效果。
莉奈娅采矿须携带莉奈娅；点位脚本指定的角色也须在出战队伍中。用本机“挖矿”页或以下高层接口；执行期间只查询状态，
不要另发移动、瞄准或射击脚本。

- `GET /mining/routes` 读取路线 ID、作者、矿种 `category`、地区 `region`、所需动作 `capabilities`、缓存状态、不支持点位和 `lastRun` 实测记录。
- `POST /mining/routes/refresh` 用 `{}` 递归更新莉奈娅、虹滴晶和萃凝晶目录；`/download` 用 `{"id":"..."}`
  下载并检查，`/import` 用 `{"path":"C:\\routes"}` 导入本地 JSON 或文件夹。
- 只选已下载且 `issues` 为空的路线。先在矿点用
  `POST /mining/start` 的 `{"routeIds":[],"stationary":true}` 试采；路线任务用
  `{"routeIds":["已检查的ID"]}`，按请求顺序各运行一次。
- 开始前确认处于游戏主界面。传送失败后 Scanner 会尝试恢复世界界面；重试前仍须观察现场，未恢复时先关闭残留菜单。
- 导航或战斗报错后，存在敌人、血量偏低或仍在空中时会尝试返回星落湖神像，逐人核验生命值；结果查看 `recovery`。恢复失败时尽可能原地暂停。用户停止或失去游戏焦点后不会继续传送。
- 批量控制器仅在失败回执 `recovery.outcome` 为 `ready`（地面、队员存活及当前生命值已确认）或 `healed`（神像处逐人核验已恢复）时进入下一条路线；该失败路线仍保留失败状态。`paused`、`failed`、未知状态、用户停止或焦点丢失时先检查现场。
- 切人须确认世界界面与目标角色生命条；死亡、低生命条或界面不明确时停止按槽位键，避免弹出食物复活窗口。切人等待、导航和瞄准均检查生命值；导航中角色意外改变也会停止并按回执处理恢复。
- `issues` 为空只表示指令已适配。查看 `lastRun` 区分走完且有收获提示、走完但收获未确认、失败和停止；路线或引擎版本变化后旧记录失效。
- 通用采矿 `strategy` 可用 `auto`、`linnea`、`zhongli`、`claymore`；auto 的通用地面采矿使用莉奈娅，水下路线使用已校验的螃蟹技能。固定队伍为玛薇卡、迪希雅、莉奈娅、钟离，路线写明角色时仍按路线执行；钟离护盾和明确指定的大剑策略保留。
- 水下先跑对应 A00 学习技能路线。观察确认已借到重甲蟹技能后，用 `{"routeIds":[],"stationary":true,"calibrateCrab":true}` 保存本机图标参照，再运行采矿路线。参照只用于识别，不能授予技能；错误技能会停止，不在水下轮转地面护盾。
- 水下原地试采用 `{"routeIds":[],"stationary":true,"strategy":"auto"}`；自动模式使用已校验的螃蟹技能，其他原地策略在水下会被拒绝。
- 地面移动路线也可能进入洞窟；未指定楼层时按图像确认地层，重叠地层不明确则停止。本地路线可用 `info.floor` 限定楼层。旧日之海、远古圣山、龙附身和额外角色脚本仍列出限制，不宣称所有社区路线都能完成。
- 202 后轮询 `/status`，用当前 jobId 查询 `/result` 或 `/stop`。网络超时先查状态，
  不自动重发开始请求。右键也可停止；焦点丢失会结束任务。
- 检查每个矿点的 `outcome`、`shots`、`loot` 和 `error`。射箭不等于收获，
  `unconfirmed` 表示未读到收获提示，`noOre` 表示未识别到矿石。需要精确总量时，
  对比背包前后数量；提示识别可能漏读或重复计数。最新报告保存在 `data/mining/last-report.json`。
- 掉落等待期间也会持续检查矿物 F 提示，避免技能动画结束后才出现的掉落被漏捡；只拾取当前选中且名称为矿物的提示。

客户端命令 `mining-routes`、`mining-refresh`、`mining-download`、`mining-import`
对应路线库；`mine request.json` 提交任务。普通玩家无需操作网页。

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

Windows 本地游戏窗口尺寸异常、普通接口因非16:9无法初始化时，可用
`{"requestId":"recover-display-one","key":"fullscreen","ocr":[]}` 执行固定
Alt+Enter切换。必须 `holdMs=0`、OCR为空；仅识别到的本地游戏窗口可用，云游戏不支持。
接口检查前台焦点与取消状态，结束、出错或异常展开均尝试松开两个键，并返回新尺寸。
这是切换动作，结果未知时查原回执并观察，不重复盲发；随后在游戏图像设置中选择
需要的分辨率。普通输入和扫描仍要求16:9。

一次一个键，从短按试走。传送后先确认目的地、加载完成与交互提示，再靠近并按F。
失去焦点、右键中断或 `/stop` 结束长按并尝试松键；正常返回、出错及异常展开也会
松键。进程被强制结束或系统故障不属于正常回执保证。

`/input/key` 与长按请求立即返回202，期间可查询或停止。取
`GET /input/result?jobId=请求ID` 至 `state=completed`，检查 feedback 与各OCR错误。
`clicked=true`只表示输入已提交，不证明打造/强化成功；`null`表示可能有部分输入，
先观察现场。Windows 文本替换用点击的 `text`，1–64个可打印字符，不能与key共用。

菜单点按未生效时，先确认仍在可接收输入的页面；可用独立`/input/key`做一次
短时按下（如80ms）并观察结果，不连续发送切换键。主界面图标可能需要鼠标模式，
不要把未生效的图标点击当作已打开页面。

## 落盘与恢复

提交前生成唯一 requestId 并保存完整请求。同 ID 同内容去重，不同内容409。
仅当前进程最近64项回执可用。服务重启或过期后先观察，不能依赖旧 ID 重发。
扫描/穿戴/锁定没有去重保证，结果未知时先查状态和现场。

附带客户端遇网络异常不重发 POST；已有输入台账再次执行只查询原回执，404后
需要人工判断现场。默认台账目录含用户数据，不提交到公共仓库。

Windows偶发文件占用只允许对本地台账的原子替换做有限重试。台账落盘失败时
不得提交尚未发送的POST；响应落盘失败也不能重发已提交操作。客户端不会重试HTTP。
