use super::routes::{download, write_cache};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use std::path::Path;

const REVISION: &str = "cc3099a1a8e6470bdbd649dd232aa06011cd60a0";

#[derive(Clone, Deserialize)]
pub(super) struct TeleportAnchor {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    pub country: String,
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
        let anchor = self.nearest((-82.18768, 3310.141))?;
        ensure!(
            anchor.kind == "Goddess",
            "Recovery destination is not a statue"
        );
        Ok(anchor)
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
}
