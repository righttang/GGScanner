use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

const REPOSITORY: &str = "https://api.github.com/repos/babalae/bettergi-scripts-list";
const MINERAL_DIR: &str = "repo/pathing/矿物";
const COLLECTIONS: &[&str] = &["莉奈娅一条龙", "虹滴晶", "萃凝晶"];
const LIMIT: u64 = 5 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub info: RouteInfo,
    pub positions: Vec<Waypoint>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RouteInfo {
    pub name: String,
    #[serde(default)]
    pub authors: Vec<Author>,
    #[serde(default, alias = "mapName")]
    pub map_name: String,
    #[serde(default, alias = "mapMatchMethod")]
    pub map_match_method: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub environment: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Author {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Waypoint {
    #[serde(default)]
    pub id: usize,
    pub x: f64,
    pub y: f64,
    #[serde(rename = "type")]
    pub kind: String,
    pub move_mode: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub action_params: String,
}

impl Waypoint {
    pub fn shots(&self) -> Result<(usize, usize)> {
        if self.action_params.is_empty() {
            return Ok((1, 1));
        }
        let values = self
            .action_params
            .split(',')
            .map(str::parse::<usize>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ensure!(
            !values.is_empty() && values.len() <= 2 && values.iter().all(|v| (1..=999).contains(v)),
            "Invalid linnea_mining parameters"
        );
        Ok((
            values[0],
            values.get(1).copied().unwrap_or(values[0]).max(values[0]),
        ))
    }
}

impl Route {
    pub fn floor_hint(&self) -> Option<i32> {
        // Ground describes the movement environment, not map elevation: a
        // walking route can enter a cave. Only explicit metadata pins a floor.
        self.info.floor
    }
    pub fn environment(&self) -> &str {
        if !self.info.environment.is_empty() {
            return &self.info.environment;
        }
        if self.info.name.contains("水下") || self.info.description.contains("螃蟹技能") {
            "underwater"
        } else if self.info.name.contains("地下") {
            "underground"
        } else {
            "ground"
        }
    }
    fn source_hint(&mut self, path: &str) {
        if self.info.environment.is_empty() {
            if path.contains("水下") {
                self.info.environment = "underwater".into();
            } else if path.contains("地下") {
                self.info.environment = "underground".into();
            }
        }
    }
    pub fn required_capabilities(&self) -> Vec<String> {
        let mut capabilities = std::collections::BTreeSet::<String>::new();
        capabilities.insert(self.environment().into());
        for p in &self.positions {
            capabilities.insert(
                match p.move_mode.as_str() {
                    "fly" => "glide",
                    "climb" => "climb",
                    "swim" => "swim",
                    "jump" => "jump",
                    _ => "ground",
                }
                .into(),
            );
            if !p.action.is_empty() {
                capabilities.insert(p.action.clone());
            }
            if p.action == "combat_script" {
                if let Ok(actions) = super::actions::parse_script(&p.action_params) {
                    for action in actions {
                        if let super::actions::Action::Select { role } = action {
                            capabilities.insert(format!("role:{}", role.name()));
                        }
                    }
                }
            }
        }
        capabilities.into_iter().collect()
    }
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() as u64 <= LIMIT, "Route exceeds 5 MiB");
        let route: Self =
            serde_json::from_slice(bytes.strip_prefix(&[239, 187, 191]).unwrap_or(bytes))?;
        ensure!(
            !route.info.name.trim().is_empty()
                && !route.positions.is_empty()
                && route.positions.len() <= 10000,
            "Route needs a name and 1–10000 waypoints"
        );
        ensure!(
            route.positions.iter().all(|p| p.x.is_finite()
                && p.y.is_finite()
                && p.x.abs() < 100000.0
                && p.y.abs() < 100000.0),
            "Invalid route coordinates"
        );
        Ok(route)
    }

    pub fn issues(&self) -> Vec<String> {
        let mut issues = Vec::new();
        if !matches!(self.info.map_name.as_str(), "" | "Teyvat") {
            issues.push(format!("Unsupported map: {}", self.info.map_name));
        }
        if !matches!(self.environment(), "ground" | "underwater" | "underground") {
            issues.push(format!(
                "Navigation capability pending: {}",
                self.environment()
            ));
        }
        if self.positions[0].kind != "teleport" {
            issues.push("First waypoint must be a teleport".into());
        }
        for (index, p) in self.positions.iter().enumerate() {
            if p.action == "combat_script" {
                match super::actions::parse_script(&p.action_params) {
                    Err(error) => {
                        issues.push(format!("Waypoint {}: {error}", index + 1));
                        continue;
                    },
                    Ok(_) => {},
                }
            }
            let valid = matches!(
                p.kind.as_str(),
                "teleport" | "path" | "target" | "orientation"
            ) && matches!(
                p.move_mode.as_str(),
                "walk" | "run" | "dash" | "jump" | "fly" | "climb" | "swim"
            ) && matches!(
                p.action.as_str(),
                "" | "linnea_mining"
                    | "combat_script"
                    | "mining"
                    | "pick_around"
                    | "stop_flying"
                    | "fight"
                    | "force_tp"
            ) && (p.action != "linnea_mining" || p.shots().is_ok())
                && (p.action != "mining"
                    || matches!(p.action_params.as_str(), "" | "disablePickupAround"))
                && (p.action != "pick_around"
                    || p.action_params.is_empty()
                    || p.action_params
                        .parse::<usize>()
                        .is_ok_and(|n| (1..=3).contains(&n)))
                && (p.action != "" || p.action_params.is_empty());
            let valid = valid
                && match p.action.as_str() {
                    "stop_flying" => {
                        p.action_params.is_empty()
                            || p.action_params.parse::<u64>().is_ok_and(|v| v <= 5000)
                    },
                    "fight" | "force_tp" => p.action_params.is_empty(),
                    _ => true,
                };
            if !valid {
                issues.push(format!(
                    "Waypoint {}: {} · {} · {} ({})",
                    index + 1,
                    p.kind,
                    p.move_mode,
                    p.action,
                    p.action_params
                ));
            }
        }
        issues
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteEntry {
    pub id: String,
    pub name: String,
    pub authors: Vec<String>,
    pub revision: String,
    pub downloaded: bool,
    pub issues: Vec<String>,
    pub points: usize,
    #[serde(default)]
    pub source_path: Option<String>,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<RouteValidation>,
}

pub const ENGINE_REVISION: &str = "mining-20261119-v10";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteValidation {
    pub engine_revision: String,
    pub route_fingerprint: String,
    pub finished_at: u64,
    pub outcome: String,
    pub completed_points: usize,
    pub total_points: usize,
    pub loot: BTreeMap<String, usize>,
    pub error: Option<String>,
}

fn fingerprint(bytes: &[u8]) -> String {
    let value = bytes.iter().fold(0xcbf29ce484222325u64, |hash, b| {
        (hash ^ *b as u64).wrapping_mul(0x100000001b3)
    });
    format!("{value:016x}")
}

fn category(path: &str) -> &'static str {
    if path.contains("/萃凝晶/") {
        "condessenceCrystal"
    } else if path.contains("/虹滴晶/") {
        "rainbowdropCrystal"
    } else if path.contains("/莉奈娅一条龙/") {
        "linnea"
    } else {
        "local"
    }
}
fn region(text: &str) -> &'static str {
    for (name, key) in [
        ("枫丹", "fontaine"),
        ("纳塔", "natlan"),
        ("挪德卡莱", "nodKrai"),
        ("蒙德", "mondstadt"),
        ("璃月", "liyue"),
        ("稻妻", "inazuma"),
        ("须弥", "sumeru"),
        ("至冬", "snezhnaya"),
    ] {
        if text.contains(name) {
            return key;
        }
    }
    "other"
}
fn community_path(path: &str) -> bool {
    path.ends_with(".json")
        && !path.contains('\\')
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
        && COLLECTIONS
            .iter()
            .any(|collection| path.starts_with(&format!("{MINERAL_DIR}/{collection}/")))
}
impl RouteEntry {
    fn inspect(&mut self, route: &Route) {
        self.name = route.info.name.clone();
        self.authors = route.info.authors.iter().map(|a| a.name.clone()).collect();
        self.issues = route.issues();
        self.points = route.positions.len();
        self.capabilities = route.required_capabilities();
        self.source_metadata();
    }
    fn source_metadata(&mut self) {
        let path = self.source_path.as_deref().unwrap_or("");
        if self.category.is_empty() {
            self.category = category(path).into();
        }
        if self.region.is_empty() {
            self.region = region(&format!("{path}/{}", self.name)).into();
        }
    }
}

#[derive(Clone)]
pub struct RouteStore {
    root: PathBuf,
}

impl Default for RouteStore {
    fn default() -> Self {
        Self::new(PathBuf::from("data/mining"))
    }
}

impl RouteStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    fn path(&self, id: &str) -> Result<PathBuf> {
        ensure!(
            !id.is_empty()
                && id.len() <= 80
                && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
            "Invalid route ID"
        );
        Ok(self.root.join("routes").join(format!("{id}.json")))
    }
    pub fn load(&self, id: &str) -> Result<Route> {
        Route::parse(&std::fs::read(self.path(id)?)?)
    }
    pub fn list(&self) -> Result<Vec<RouteEntry>> {
        let path = self.root.join("catalog.json");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut entries: Vec<RouteEntry> = serde_json::from_slice(&std::fs::read(path)?)?;
        for entry in &mut entries {
            entry.downloaded = self.path(&entry.id)?.is_file();
            if entry.downloaded {
                match self.load(&entry.id) {
                    Ok(route) => entry.inspect(&route),
                    Err(error) => {
                        entry.issues = vec![format!("Cached route could not be read: {error}")]
                    },
                }
            }
            entry.last_run = std::fs::read(
                self.root
                    .join("validation")
                    .join(format!("{}.json", entry.id)),
            )
            .ok()
            .and_then(|bytes| serde_json::from_slice::<RouteValidation>(&bytes).ok())
            .filter(|run| {
                run.engine_revision == ENGINE_REVISION
                    && std::fs::read(self.path(&entry.id).unwrap())
                        .is_ok_and(|bytes| fingerprint(&bytes) == run.route_fingerprint)
            });
            entry.source_metadata();
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }
    fn save_list(&self, entries: &[RouteEntry]) -> Result<()> {
        write_cache(
            &self.root.join("catalog.json"),
            &serde_json::to_vec_pretty(entries)?,
        )
    }
    fn save_route(&self, id: &str, route: &Route) -> Result<()> {
        write_cache(&self.path(id)?, &serde_json::to_vec_pretty(route)?)
    }
    pub fn record_run(&self, id: &str, mut validation: RouteValidation) -> Result<()> {
        validation.route_fingerprint = fingerprint(&std::fs::read(self.path(id)?)?);
        validation.engine_revision = ENGINE_REVISION.into();
        write_cache(
            &self.root.join("validation").join(format!("{id}.json")),
            &serde_json::to_vec_pretty(&validation)?,
        )
    }
    pub fn import(&self, path: &Path) -> Result<Vec<RouteEntry>> {
        let mut paths = if path.is_dir() {
            std::fs::read_dir(path)?
                .filter_map(|p| p.ok().map(|p| p.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
                .collect::<Vec<_>>()
        } else {
            vec![path.to_owned()]
        };
        paths.sort();
        ensure!(paths.len() <= 500, "Import at most 500 routes at a time");
        // Parse the entire selection before writing any route.
        let routes = paths
            .iter()
            .map(|p| Route::parse(&std::fs::read(p)?).with_context(|| p.display().to_string()))
            .collect::<Result<Vec<_>>>()?;
        let mut entries = self.list()?;
        for (mut route, path) in routes.into_iter().zip(&paths) {
            route.source_hint(&path.to_string_lossy());
            let id = uuid::Uuid::new_v4().to_string();
            self.save_route(&id, &route)?;
            entries.push(RouteEntry {
                id,
                name: route.info.name.clone(),
                authors: route.info.authors.iter().map(|a| a.name.clone()).collect(),
                revision: "local".into(),
                downloaded: true,
                issues: route.issues(),
                points: route.positions.len(),
                source_path: None,
                category: category(&path.to_string_lossy().replace('\\', "/")).into(),
                region: region(&format!("{}/{}", path.display(), route.info.name)).into(),
                capabilities: route.required_capabilities(),
                last_run: None,
            });
        }
        self.save_list(&entries)?;
        Ok(entries)
    }
    pub fn refresh(&self) -> Result<Vec<RouteEntry>> {
        let commit: serde_json::Value =
            serde_json::from_slice(&download(&format!("{REPOSITORY}/commits/main"))?)?;
        let revision = commit["sha"]
            .as_str()
            .context("Missing community revision")?;
        ensure!(
            revision.len() == 40 && revision.bytes().all(|c| c.is_ascii_hexdigit()),
            "Invalid community revision"
        );
        let mut url = reqwest::Url::parse(&format!("{REPOSITORY}/contents/"))?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Invalid catalog URL"))?
            .extend(MINERAL_DIR.split('/'));
        url.query_pairs_mut().append_pair("ref", revision);
        let listing: Vec<serde_json::Value> = serde_json::from_slice(&download(url.as_str())?)?;
        let old = self.list()?;
        let mut previous: BTreeMap<String, RouteEntry> =
            old.into_iter().map(|r| (r.id.clone(), r)).collect();
        let mut entries: BTreeMap<String, RouteEntry> = previous
            .iter()
            .filter(|(_, r)| r.source_path.is_none())
            .map(|(id, r)| (id.clone(), r.clone()))
            .collect();
        for collection in COLLECTIONS {
            let Some(folder) = listing
                .iter()
                .find(|file| file["name"].as_str() == Some(collection))
            else {
                continue;
            };
            let sha = folder["sha"]
                .as_str()
                .context("Missing collection tree ID")?;
            ensure!(
                sha.len() == 40 && sha.bytes().all(|c| c.is_ascii_hexdigit()),
                "Invalid collection tree ID"
            );
            let tree: serde_json::Value = serde_json::from_slice(&download(&format!(
                "{REPOSITORY}/git/trees/{sha}?recursive=1"
            ))?)?;
            ensure!(
                tree["truncated"].as_bool() != Some(true),
                "Community collection tree was truncated"
            );
            for file in tree["tree"].as_array().context("Missing collection tree")? {
                let relative = file["path"].as_str().unwrap_or("");
                let source = format!("{MINERAL_DIR}/{collection}/{relative}");
                if file["type"] != "blob" || !community_path(&source) {
                    continue;
                }
                let sha = file["sha"].as_str().context("Missing route blob ID")?;
                ensure!(
                    sha.len() == 40 && sha.bytes().all(|c| c.is_ascii_hexdigit()),
                    "Invalid route blob ID"
                );
                let id = format!("community-{sha}");
                if entries.contains_key(&id) {
                    continue;
                }
                let mut entry = previous.remove(&id).unwrap_or(RouteEntry {
                    id: id.clone(),
                    name: relative
                        .rsplit('/')
                        .next()
                        .unwrap_or(relative)
                        .trim_end_matches(".json")
                        .into(),
                    source_path: Some(source.clone()),
                    ..Default::default()
                });
                entry.revision = revision.into();
                entry.source_path = Some(source);
                entry.source_metadata();
                entries.insert(id, entry);
            }
        }
        let entries: Vec<_> = entries.into_values().collect();
        self.save_list(&entries)?;
        Ok(entries)
    }
    pub fn fetch(&self, id: &str) -> Result<Vec<RouteEntry>> {
        let mut entries = self.list()?;
        let entry = entries
            .iter_mut()
            .find(|e| e.id == id)
            .context("Unknown route")?;
        if !entry.downloaded {
            let path = entry.source_path.as_deref().context("No download source")?;
            ensure!(community_path(path), "Invalid community path");
            let mut url = reqwest::Url::parse(
                "https://raw.githubusercontent.com/babalae/bettergi-scripts-list/",
            )?;
            url.path_segments_mut()
                .unwrap()
                .push(&entry.revision)
                .extend(path.split('/'));
            let mut route = Route::parse(&download(url.as_str())?)?;
            route.source_hint(path);
            self.save_route(id, &route)?;
            entry.inspect(&route);
            entry.downloaded = true;
            self.save_list(&entries)?;
        }
        Ok(entries)
    }
}

pub(crate) fn download(url: &str) -> Result<Vec<u8>> {
    let response = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(90))
        .build()?
        .get(url)
        .header("User-Agent", "GGScanner-mining")
        .send()?
        .error_for_status()?;
    let mut bytes = Vec::new();
    response
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 * 1024 {
        bail!("Download exceeds 64 MiB");
    }
    Ok(bytes)
}

pub(crate) fn write_cache(path: &Path, data: &[u8]) -> Result<()> {
    std::fs::create_dir_all(path.parent().context("Cache needs a parent directory")?)?;
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temp, data)?;
    let result = std::fs::rename(&temp, path);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    Ok(result?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ground_routes_do_not_implicitly_exclude_cave_floors() {
        let mut route = Route::parse(br#"{"info":{"name":"ground route"},"positions":[{"id":1,"x":1,"y":1,"type":"teleport","move_mode":"walk"}]}"#).unwrap();
        assert_eq!(route.environment(), "ground");
        assert_eq!(route.floor_hint(), None);
        route.info.floor = Some(-1);
        assert_eq!(route.floor_hint(), Some(-1));
        route.info.floor = Some(0);
        assert_eq!(route.floor_hint(), Some(0));
    }
    #[test]
    fn edited_routes_invalidate_previous_live_evidence() {
        let root =
            std::env::temp_dir().join(format!("ggscanner-evidence-{}", uuid::Uuid::new_v4()));
        let store = RouteStore::new(root.clone());
        let mut route = route("path", "combat_script", "wait(1)");
        store.save_route("case", &route).unwrap();
        store
            .save_list(&[RouteEntry {
                id: "case".into(),
                ..Default::default()
            }])
            .unwrap();
        store
            .record_run(
                "case",
                RouteValidation {
                    engine_revision: String::new(),
                    route_fingerprint: String::new(),
                    finished_at: 1,
                    outcome: "failed".into(),
                    completed_points: 1,
                    total_points: 2,
                    loot: Default::default(),
                    error: Some("test failure".into()),
                },
            )
            .unwrap();
        assert_eq!(
            store.list().unwrap()[0].last_run.as_ref().unwrap().outcome,
            "failed"
        );
        route.positions[1].action_params = "wait(2)".into();
        store.save_route("case", &route).unwrap();
        assert!(store.list().unwrap()[0].last_run.is_none());
        std::fs::remove_file(store.path("case").unwrap()).unwrap();
        std::fs::remove_file(root.join("validation/case.json")).unwrap();
        std::fs::remove_file(root.join("catalog.json")).unwrap();
        std::fs::remove_dir(root.join("routes")).unwrap();
        std::fs::remove_dir(root.join("validation")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    fn route(kind: &str, action: &str, params: &str) -> Route {
        Route {
            info: RouteInfo {
                name: "test".into(),
                authors: vec![],
                map_name: "Teyvat".into(),
                map_match_method: "TemplateMatch".into(),
                ..Default::default()
            },
            positions: vec![
                Waypoint {
                    id: 1,
                    x: 1.0,
                    y: 2.0,
                    kind: "teleport".into(),
                    move_mode: "walk".into(),
                    action: "".into(),
                    action_params: "".into(),
                },
                Waypoint {
                    id: 2,
                    x: 2.0,
                    y: 3.0,
                    kind: kind.into(),
                    move_mode: "walk".into(),
                    action: action.into(),
                    action_params: params.into(),
                },
            ],
        }
    }
    #[test]
    fn validates_supported_actions_and_rejects_unknown_scripts() {
        assert!(route("orientation", "linnea_mining", "2,4")
            .issues()
            .is_empty());
        assert!(route("path", "combat_script", "wait(3)")
            .issues()
            .is_empty());
        assert!(!route("path", "combat_script", "execute(system)")
            .issues()
            .is_empty());
        assert!(route("path", "combat_script", "钟离 e(hold)")
            .issues()
            .is_empty());
        assert!(route("target", "mining", "").issues().is_empty());
        assert!(!route("target", "pick_around", "999").issues().is_empty());
        assert!(!route("path", "linnea_mining", "2.2").issues().is_empty());
        assert_eq!(
            route("orientation", "linnea_mining", "3,1").positions[1]
                .shots()
                .unwrap(),
            (3, 3)
        );
    }
    #[test]
    fn route_ids_cannot_escape_store() {
        let store = RouteStore::default();
        assert!(store.load("../secret").is_err());
    }
    #[test]
    fn classifies_nested_catalogs_and_rejects_paths_outside_collections() {
        let path = "repo/pathing/矿物/萃凝晶/枫丹水下@作者/A01.json";
        assert!(community_path(path));
        assert_eq!(category(path), "condessenceCrystal");
        assert_eq!(region(path), "fontaine");
        for path in [
            "repo/js/example.json",
            "repo/pathing/矿物/萃凝晶/../secret.json",
            "repo/pathing/矿物/萃凝晶/README.md",
            "repo/pathing/矿物/萃凝晶/a\\b.json",
        ] {
            assert!(!community_path(path), "{path}");
        }
    }
    #[test]
    fn underwater_context_survives_serialization_with_its_required_capability() {
        let mut route = route("path", "combat_script", "wait(3)");
        route.source_hint("repo/pathing/矿物/萃凝晶/枫丹水下/A01.json");
        let round_trip = Route::parse(&serde_json::to_vec(&route).unwrap()).unwrap();
        assert_eq!(round_trip.environment(), "underwater");
        assert!(round_trip
            .required_capabilities()
            .contains(&"underwater".into()));
        assert!(round_trip.issues().is_empty());
        route.info.map_name = "SeaOfBygoneEras".into();
        assert!(route
            .issues()
            .iter()
            .any(|issue| issue.contains("Unsupported map")));
    }
    #[test]
    fn malformed_cached_route_does_not_hide_other_routes() {
        let root = std::env::temp_dir().join(format!("ggscanner-mining-{}", uuid::Uuid::new_v4()));
        let store = RouteStore::new(root.clone());
        let good = route("path", "combat_script", "wait(3)");
        store.save_route("valid", &good).unwrap();
        write_cache(&store.path("broken").unwrap(), b"not json").unwrap();
        store
            .save_list(&[
                RouteEntry {
                    id: "valid".into(),
                    ..Default::default()
                },
                RouteEntry {
                    id: "broken".into(),
                    ..Default::default()
                },
            ])
            .unwrap();
        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries
            .iter()
            .find(|e| e.id == "valid")
            .unwrap()
            .issues
            .is_empty());
        assert!(entries.iter().find(|e| e.id == "broken").unwrap().issues[0]
            .contains("could not be read"));
        std::fs::remove_file(store.path("valid").unwrap()).unwrap();
        std::fs::remove_file(store.path("broken").unwrap()).unwrap();
        std::fs::remove_file(root.join("catalog.json")).unwrap();
        std::fs::remove_dir(root.join("routes")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
