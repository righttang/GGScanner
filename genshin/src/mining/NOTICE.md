The optional mining resources are fetched from BetterGI's public repositories.

- Community routes and author metadata come from https://github.com/babalae/bettergi-scripts-list.
- The ore ONNX model and map atlas come from https://github.com/babalae/bettergi-libraries at revision 13937839dba42df5384940e3cff2e4eff7e29e19.
- Route coordinate conventions and action parameters follow https://www.bettergi.com/dev/pathing-dev.html.
- Teleport icon types, map coordinates and landing coordinates come from `GameTask/AutoTrackPath/Assets/tp.json` in https://github.com/babalae/better-genshin-impact at revision cc3099a1a8e6470bdbd649dd232aa06011cd60a0.
- Movement keycaps, interaction keycaps and the map layer button template come
  from that same pinned BetterGI revision. Layer manifests accompany the map atlas.
- Big-map feature extraction uses the MIT-licensed `sift-features` Rust crate,
  pinned to version 0.1.0 by Cargo.lock.

These resources are cached locally on demand. Runtime control, route parsing,
template correlation and inference integration are implemented in GGScanner.
