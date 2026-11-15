use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

const REPOSITORY: &str = "https://api.github.com/repos/babalae/bettergi-scripts-list";
const ROUTE_DIR: &str = "repo/pathing/矿物/莉奈娅一条龙";
const LIMIT: u64 = 5 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub info: RouteInfo,
    pub positions: Vec<Waypoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteInfo {
    pub name: String,
    #[serde(default)]
    pub authors: Vec<Author>,
    #[serde(default, alias = "mapName")]
    pub map_name: String,
    #[serde(default, alias = "mapMatchMethod")]
    pub map_match_method: String,
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
        if !matches!(self.info.map_name.as_str(), "" | "Teyvat") || self.info.name.contains("地下")
        {
            issues.push("Teyvat surface routes only".into());
        }
        if self.positions[0].kind != "teleport" {
            issues.push("First waypoint must be a teleport".into());
        }
        for (index, p) in self.positions.iter().enumerate() {
            let valid = matches!(
                p.kind.as_str(),
                "teleport" | "path" | "target" | "orientation"
            ) && matches!(p.move_mode.as_str(), "walk" | "run" | "dash")
                && matches!(p.action.as_str(), "" | "linnea_mining")
                && (p.action != "linnea_mining" || p.shots().is_ok())
                && (p.action != "" || p.action_params.is_empty());
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
                entry.issues = self.load(&entry.id)?.issues();
            }
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
        for route in routes {
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
            .extend(ROUTE_DIR.split('/'));
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
        for file in listing {
            let name = file["name"].as_str().unwrap_or("");
            if !name.ends_with(".json") {
                continue;
            }
            let sha = file["sha"].as_str().context("Missing route blob ID")?;
            let id = format!("community-{sha}");
            let mut entry = previous.remove(&id).unwrap_or(RouteEntry {
                id: id.clone(),
                name: name.trim_end_matches(".json").into(),
                authors: Vec::new(),
                revision: revision.into(),
                downloaded: false,
                issues: Vec::new(),
                points: 0,
                source_path: Some(format!("{ROUTE_DIR}/{name}")),
            });
            entry.revision = revision.into();
            entries.insert(id, entry);
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
            ensure!(
                path.starts_with(&format!("{ROUTE_DIR}/")) && !path.contains(".."),
                "Invalid community path"
            );
            let mut url = reqwest::Url::parse(
                "https://raw.githubusercontent.com/babalae/bettergi-scripts-list/",
            )?;
            url.path_segments_mut()
                .unwrap()
                .push(&entry.revision)
                .extend(path.split('/'));
            let route = Route::parse(&download(url.as_str())?)?;
            self.save_route(id, &route)?;
            entry.name = route.info.name.clone();
            entry.authors = route.info.authors.iter().map(|a| a.name.clone()).collect();
            entry.issues = route.issues();
            entry.points = route.positions.len();
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
    fn route(kind: &str, action: &str, params: &str) -> Route {
        Route {
            info: RouteInfo {
                name: "test".into(),
                authors: vec![],
                map_name: "Teyvat".into(),
                map_match_method: "TemplateMatch".into(),
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
    fn validate_orientation_and_reject_scripts() {
        assert!(route("orientation", "linnea_mining", "2,4")
            .issues()
            .is_empty());
        assert!(!route("path", "combat_script", "wait(3)")
            .issues()
            .is_empty());
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
}
