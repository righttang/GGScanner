---
name: ggscanner-artifact-manager
description: Use GGScanner and GGArtifact to scan Genshin inventory, plan independent artifact loadouts, apply equipment and party presets, and manage authorized enhancement or forging budgets. Use for account gear management rather than scanner development.
---

# GGScanner 圣遗物整理

把用户当前库存整理为可执行、可核验的配装，并按用户预算培养。保留现有成果与
历史台账；不继承另一个用户的预算、等级门槛或队伍偏好。

## 恢复现场

读取会话授权、最新 GOOD、方案与回执。明确角色范围、优先体系、是否共用装备，
以及强化/打造/重塑/精炼和材料的约束。已有答案不重复询问；只有影响不可逆操作
且无法从授权判断的缺项才澄清。

先查 `/health`、`/status`。默认端口18765；WSL访问Windows用 `curl.exe`。
Windows HDR与扫描设置一致。启停见仓库 `docs/WINDOWS_LOCAL_CONTROL.md`；
不要自行安装管理员入口。

## 流程与资源

- 扫描、独立分配、培养和核验：读 [inventory-workflow.md](references/inventory-workflow.md)。
- 接口、去重、移动与现场：读 [game-api.md](references/game-api.md)。
- [ggscanner_client.py](scripts/ggscanner_client.py) 在POST前保存请求及结果；
  `--dry-run`不操作游戏，`--help`提供示例。

## 执行约束

1. 扫描、穿戴、锁定与输入共享队列。不同时开另一套鼠标脚本；停止用当前jobId，
   等终态后才发下一项。健康、状态与停止可在任务中查询。
2. 资源操作先落台账再提交，按实际累计预算。重塑保留旧结果也计一次。
   分阶段强化再评价，不自动把全部候选拉满。
3. 超时不是未发生；输入按原requestId查回执，失效则观察现场。扫描与装备请求
   不自动重发，服务重启后也不盲目重复。
4. 在整个库存检查匹配身份唯一性。location不用于正式消歧，主词条、充能、套装、
   反应触发与实际命座均须考虑。网站分配只是计划，不能替代游戏执行回执。
5. 移动从短按开始，确认传送位置与交互提示；时长不是精确距离。结束自动松键，
   焦点丢失或中断后先观察，不盲目连发。
6. 旧配装和队伍先备份，完成后核验实际持有人、成员顺序及队名。预设复用角色
   不代表允许共用装备；超过游戏容量的额外方案另存。

## 交付

报告实际完成数量、消耗、核验方法、失败/过渡项及培养缺口，附最新实际GOOD与
原方案备份。基线加受控变更不称为重新全扫描，独立穿戴成功不称为全局最优。
材料表注明配方总量或扣库存的实际缺口。

交接未完成jobId、现场、预算和未知结果。账号导出、截图与台账留在用户本地；
公开分享仅包含通用代码和说明。
