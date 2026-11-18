use super::routes::{download, write_cache};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use std::path::Path;

const REVISION: &str = "cc3099a1a8e6470bdbd649dd232aa06011cd60a0";

#[derive(Clone, Deserialize)]
pub(super) struct TeleportAnchor {
    #[serde(default)]
    id: u64,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    areas: Vec<String>,
    position: [f64; 3],
    #[serde(rename = "tranPosition")]
    landing: Option<[f64; 3]>,
    #[serde(skip)]
    neighbor_distance: f64,
}
impl TeleportAnchor {
    pub fn correction_limit(&self, scale: f64) -> f64 {
        (self.neighbor_distance / (4.0 * scale) * 0.3).clamp(6.0, 36.0)
    }
    pub fn icon(&self) -> &str {
        match self.kind.as_str() {
            "Goddess" => "StatueOfTheSeven.png",
            "SubSpaceWaypoint" => "SubSpaceWaypoint.png",
            "PortableWaypoint" => "PortableWaypoint.png",
            "NodKraiMeetingPoint" | "SnezhnayaMeetingPoint" => "NodKraiMeetingPoint.png",
            "Domain" => "Domain.png",
            _ => "TeleportWaypoint.png",
        }
    }
    pub fn map_position(&self) -> (f64, f64) {
        (self.position[2], self.position[0])
    }
    pub fn landing_position(&self) -> (f64, f64) {
        let position = self.landing.unwrap_or(self.position);
        (position[2], position[0])
    }
    pub fn matches_menu(&self, text: &str) -> bool {
        match self.kind.as_str() {
            "TeleportWaypoint" | "SubSpaceWaypoint" | "PortableWaypoint" => text.contains("传送"),
            "Goddess" => text.contains("神像"),
            "NodKraiMeetingPoint" | "SnezhnayaMeetingPoint" => text.contains("聚所"),
            _ => !self.name.is_empty() && text.contains(&self.name),
        }
    }
}

pub(super) struct TeleportCatalog(Vec<TeleportAnchor>);
impl TeleportCatalog {
    pub fn recovery_statue(&self) -> Result<&TeleportAnchor> {
        self.0
            .iter()
            .find(|anchor| {
                anchor.id == 7
                    && anchor.kind == "Goddess"
                    && anchor.country.contains("蒙德")
                    && anchor.areas.iter().any(|area| area == "星落湖")
            })
            .context("Starfell Lake statue is missing from the teleport catalog")
    }
    pub fn new() -> Result<Self> {
        let file = format!("data/mining/teleports-{REVISION}.json");
        let path = Path::new(&file);
        if !path.is_file() {
            write_cache(path,&download(&format!("https://raw.githubusercontent.com/babalae/better-genshin-impact/{REVISION}/BetterGenshinImpact/GameTask/AutoTrackPath/Assets/tp.json"))?)?;
        }
        Self::parse(&std::fs::read(path)?)
    }
    fn parse(bytes: &[u8]) -> Result<Self> {
        #[derive(Deserialize)]
        struct Catalog {
            data: Vec<Scene>,
        }
        #[derive(Deserialize)]
        struct Scene {
            #[serde(rename = "mapName")]
            name: String,
            points: Vec<TeleportAnchor>,
        }
        let data: Catalog =
            serde_json::from_slice(bytes.strip_prefix(&[239, 187, 191]).unwrap_or(bytes))?;
        let mut points = data
            .data
            .into_iter()
            .find(|scene| scene.name == "Teyvat")
            .context("Teyvat teleport catalog is missing")?
            .points;
        for i in 0..points.len() {
            let p = points[i].map_position();
            points[i].neighbor_distance = points
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, a)| {
                    let q = a.map_position();
                    (p.0 - q.0).hypot(p.1 - q.1)
                })
                .fold(f64::INFINITY, f64::min);
        }
        Ok(Self(points))
    }
    pub fn nearest(&self, point: (f64, f64)) -> Result<&TeleportAnchor> {
        let distance = |anchor: &&TeleportAnchor| {
            let p = anchor.landing_position();
            (p.0 - point.0).hypot(p.1 - point.1)
        };
        let anchor = self
            .0
            .iter()
            .min_by(|a, b| distance(a).total_cmp(&distance(b)))
            .context("No teleport anchors")?;
        ensure!(
            distance(&anchor) < 40.0,
            "Route teleport does not match a known landing point"
        );
        Ok(anchor)
    }
    pub fn route_anchor(&self, point: (f64, f64)) -> Result<&TeleportAnchor> {
        if let Ok(anchor) = self.nearest(point) {
            return Ok(anchor);
        }
        // Community teleport waypoints are selector hints, sometimes recorded
        // after moving away from the spawn. Resolve only a clearly isolated
        // nearby anchor; the runtime still verifies its canonical icon/landing.
        let mut candidates: Vec<_> = self
            .0
            .iter()
            .map(|anchor| {
                let distance = |p: (f64, f64)| (p.0 - point.0).hypot(p.1 - point.1);
                (
                    distance(anchor.landing_position()).min(distance(anchor.map_position())),
                    anchor,
                )
            })
            .collect();
        candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
        let &(distance, anchor) = candidates.first().context("No teleport anchors")?;
        let next = candidates.get(1).map_or(f64::INFINITY, |p| p.0);
        ensure!(
            distance < 80.0 && distance * 3.0 < next && distance * 4.0 < anchor.neighbor_distance,
            "Route teleport hint is ambiguous or too far from a known anchor"
        );
        Ok(anchor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlapping_workshop_and_anchor_select_the_correct_destination() {
        let catalog=TeleportCatalog::parse(br#"{"data":[{"mapName":"Teyvat","points":[
          {"type":"NodKraiMeetingPoint","name":"workshop","position":[1643.597,204.868,9951.072],"tranPosition":[1640.9175,204.97887,9955.294]},
          {"type":"TeleportWaypoint","position":[1717.405,261.1723,9873.979],"tranPosition":[1718.958,261.273,9885.505]}
        ]}]}"#).unwrap();
        let workshop = catalog.nearest((9955.292, 1640.8062)).unwrap();
        assert!(workshop.matches_menu("「聚所·叮铃哐啷蛋卷工坊」"));
        assert!(!workshop.matches_menu("传送锚点"));
        assert_eq!(workshop.map_position(), (9951.072, 1643.597));
        assert!(catalog.nearest((0.0, 0.0)).is_err());
    }
    #[test]
    fn shifted_route_hint_resolves_to_the_canonical_landing_without_relaxing_confirmation() {
        let catalog = TeleportCatalog::parse(br#"{"data":[{"mapName":"Teyvat","points":[
          {"type":"TeleportWaypoint","position":[2208.05,0,8838.03],"tranPosition":[2204,0,8834]},
          {"type":"TeleportWaypoint","position":[2137.506,0,9012.594],"tranPosition":[2137,0,9021]},
          {"type":"TeleportWaypoint","position":[2298.685,0,9095.593],"tranPosition":[2296.752,0,9099.543]}
        ]}]}"#).unwrap();
        let hint = (8791.0576, 2195.499);
        assert!(catalog.nearest(hint).is_err());
        let anchor = catalog.route_anchor(hint).unwrap();
        assert_eq!(anchor.landing_position(), (8834.0, 2204.0));
        assert_eq!(anchor.map_position(), (8838.03, 2208.05));
        assert!(catalog.route_anchor((8600.0, 2200.0)).is_err());
    }
    #[test]
    fn shifted_hint_must_not_choose_between_neighboring_anchors() {
        let catalog = TeleportCatalog::parse(
            br#"{"data":[{"mapName":"Teyvat","points":[
          {"type":"TeleportWaypoint","position":[0,0,0]},
          {"type":"TeleportWaypoint","position":[0,0,100]}
        ]}]}"#,
        )
        .unwrap();
        assert!(catalog.route_anchor((50.0, 0.0)).is_err());
        assert!(catalog.route_anchor((-50.0, 0.0)).is_err());
        assert_eq!(
            catalog.route_anchor((2.0, 0.0)).unwrap().landing_position(),
            (0.0, 0.0)
        );
    }
    #[test]
    fn recovery_selects_starfell_by_identity_instead_of_another_statues_coordinates() {
        let catalog=TeleportCatalog::parse(r#"{"data":[{"mapName":"Teyvat","points":[
          {"id":1885,"type":"Goddess","country":"蒙德","areas":["风息山"],"position":[3309.774,236.7597,-77.536],"tranPosition":[3310.141,237.4942,-82.18768]},
          {"id":7,"type":"Goddess","country":"蒙德","areas":["坠星山谷","星落湖"],"position":[2566.115,214.0869,-1323.474],"tranPosition":[2564.5361,215.3869,-1329.8206]}
        ]}]}"#.as_bytes()).unwrap();
        let anchor = catalog.recovery_statue().unwrap();
        assert_eq!(anchor.id, 7);
        assert_eq!(anchor.landing_position(), (-1329.8206, 2564.5361));
        let wrong=TeleportCatalog::parse(r#"{"data":[{"mapName":"Teyvat","points":[
          {"id":1885,"type":"Goddess","country":"蒙德","areas":["风息山"],"position":[3309.774,236.7597,-77.536]}
        ]}]}"#.as_bytes()).unwrap();
        assert!(wrong.recovery_statue().is_err());
    }
}
