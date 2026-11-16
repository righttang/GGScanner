The optional mining resources are fetched from BetterGI's public repositories.

- Community routes and author metadata come from https://github.com/babalae/bettergi-scripts-list.
- The ore ONNX model and map atlas come from https://github.com/babalae/bettergi-libraries at revision 13937839dba42df5384940e3cff2e4eff7e29e19.
- Route coordinate conventions and action parameters follow https://www.bettergi.com/dev/pathing-dev.html.
- Teleport icon types, map coordinates and landing coordinates come from `GameTask/AutoTrackPath/Assets/tp.json` in https://github.com/babalae/better-genshin-impact at revision cc3099a1a8e6470bdbd649dd232aa06011cd60a0.
- Movement keycaps, interaction keycaps and the map layer button template come
  from that same pinned BetterGI revision. Layer manifests accompany the map atlas.
- Big-map feature extraction uses the MIT-licensed `sift-features` Rust crate,
  pinned to version 0.1.0 by Cargo.lock.
- Precomputed global SIFT keypoints/descriptors use the same pinned map asset
  revision. Teleport icon and map-close templates use the pinned BetterGI
  revision above.
- Navigation design was reviewed against BetterGI revision
  d300886ffcad76f9f16518b43a0affb8c8aef84d: `NavigationInstance`, `TpTask`,
  `PathExecutor`, `TrapEscaper` and the map matching classes. The Rust controller,
  descriptor search, pan validation and replay driver are implemented here.
- `camera.rs` adapts BetterGI's `CameraOrientationCalculator` and the polar
  edge method in `CameraOrientationFromGia` from that revision to Rust. That
  module is GPL-3.0-or-later; the upstream license is included in
  `licenses/BetterGI-GPL-3.0.txt`. The combined work can be distributed under
  GPL version 3 as permitted by this project's GPL-2.0-or-later grant.

These resources are cached locally on demand. Runtime control, route parsing,
template correlation and inference integration are implemented in GGScanner.
