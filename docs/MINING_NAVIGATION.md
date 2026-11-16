# Native mining navigation

The Windows executor uses recorded route waypoints and visual feedback. It
does not require BetterGI to run. Upstream design references and pinned asset
revisions are listed in `genshin/src/mining/NOTICE.md`.

- `big_map.rs`: verified map UI, wide-viewport local SIFT followed by a global
  feature bank and native-reference fallback, world/screen projection,
  sent-drag direction/scale validation and masked icon refinement.
  A predicted pan centre never authorizes a teleport click.
- `runtime.rs`: map open/region selection/ground layer, recognized zoom-slider
  control and bounded zoom recovery,
  anchor panel selection and landing confirmation. Failed teleports attempt to
  restore the world UI. Party changes require the active HUD badge to agree.
  A confirmed nearby landing can continue without another teleport. Ground
  travel prefers Mavuika when present; underwater travel uses Dehya.
  Risky route failures return to the Starfell Lake statue and verify each party
  member's health. Statue approaches exit climbing and confirm a stable ground
  HUD before checking party health. A failed recovery pauses in place when possible. User stop
  and loss of foreground focus do not trigger additional recovery input.
- `vision.rs`: masked mini-map matching across world/city/underground templates,
  previous-layer priority, local expansion and global relocalization. Global
  movement recovery requires agreement across fresh frames. Camera cone tracking
  is enabled only after its rotation agrees with a measured probe.
- `camera.rs`: the BetterGI alpha-distribution and polar-edge camera algorithms,
  adapted to Rust under GPL version 3 with the upstream license retained.
- `travel.rs`: continuous forward movement, visual steering, stationary turns,
  fine approach, ground/glide/climb/underwater states and three bounded escape
  attempts. Expensive observations pause movement. Key guards release on normal
  completion, errors, cancellation and unwinding.
  Expanded/global mini-map matching runs on a captured world image while the
  Paimon menu pauses the game; successful recovery then checks fresh world frames.

Replay tests exercise the production movement controller with a simulated input
driver. Image regressions are opt-in: game screenshots stay local. Set
`GGSCANNER_NAV_FIXTURES` to a JSON array of objects containing `path`, `hint`
and expected `center` (two map coordinates each), and `tolerance` in world units;
run the ignored `regression_local_map_viewports` test from the runtime directory.
`GGSCANNER_ANCHOR_FRAME` runs the opt-in Selo anchor image regression.
`GGSCANNER_PARTY_FRAME` and zero-based `GGSCANNER_ACTIVE_SLOT` verify party badges.

Replay success is not evidence that a community route completed in game. Route
validation remains tied to route contents and the engine revision. Check live
reports for teleport, position/floor, movement and collection results separately.
Routes in unsupported scenes or requiring unavailable actions remain rejected.

Positions contain two map coordinates. Vertical movement relies on recorded
route actions and the observed glide/climb/underwater HUD state. Obstacle
recovery tries a bounded sequence of inputs; cliffs, overhangs and mismatched
route geometry can still end a run. Assess climbing and combat routes from
their individual live reports before using them unattended.
