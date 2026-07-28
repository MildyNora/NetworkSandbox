use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentStatus {
    Created,
    Running,
    Ready,
    ValidationFailed,
    Applied,
    Discarded,
}

impl std::fmt::Display for EnvironmentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_value(self).unwrap().as_str().unwrap()
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FailurePolicy {
    Warn,
    BlockApply,
    Stop,
}

impl std::str::FromStr for FailurePolicy {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "warn" => Ok(Self::Warn),
            "block-apply" => Ok(Self::BlockApply),
            "stop" => Ok(Self::Stop),
            _ => Err("expected warn, block-apply, or stop".into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentPolicy {
    pub on_failure: FailurePolicy,
    pub auto_rollback: bool,
}

impl Default for EnvironmentPolicy {
    fn default() -> Self {
        Self {
            on_failure: FailurePolicy::BlockApply,
            auto_rollback: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Inbound,
    Outbound,
    Unknown,
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inbound => write!(f, "inbound"),
            Self::Outbound => write!(f, "outbound"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationState {
    Pending,
    Preserved,
    Lost,
    Degraded,
    New,
    Unverifiable,
    Ignored,
}

impl std::fmt::Display for ValidationState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self).map(|_| ())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub id: String,
    pub name: Option<String>,
    pub protocol: String,
    pub direction: Direction,
    pub local: String,
    pub remote: String,
    pub process: Option<String>,
    #[serde(default)]
    pub probe: ProbeSpec,
    pub required: bool,
    pub validation: ValidationState,
    pub detail: Option<String>,
    pub last_checked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProbeSpec {
    Tcp {
        endpoint: String,
        #[serde(default)]
        interface: Option<String>,
    },
    Command {
        argv: Vec<String>,
    },
    External {
        description: String,
    },
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Environment {
    pub version: u32,
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub status: EnvironmentStatus,
    pub base_root: PathBuf,
    pub policy: EnvironmentPolicy,
    pub baseline: Vec<Capability>,
    pub ignored_paths: Vec<PathBuf>,
    pub applied_transaction: Option<String>,
    #[serde(default)]
    pub route_candidates: Vec<RouteCandidate>,
    #[serde(default)]
    pub origins: BTreeMap<PathBuf, Origin>,
    #[serde(default)]
    pub deleted_paths: Vec<PathBuf>,
    #[serde(default, alias = "vm_tracked_paths")]
    pub tracked_paths: Vec<PathBuf>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl Environment {
    pub fn new(
        name: String,
        description: Option<String>,
        base_root: PathBuf,
        policy: EnvironmentPolicy,
        baseline: Vec<Capability>,
    ) -> Self {
        let now = Utc::now();
        Self {
            version: STATE_VERSION,
            id: Uuid::new_v4(),
            name,
            description,
            created_at: now,
            updated_at: now,
            status: EnvironmentStatus::Created,
            base_root,
            policy,
            baseline,
            ignored_paths: vec![
                PathBuf::from("proc"),
                PathBuf::from("sys"),
                PathBuf::from("dev"),
                PathBuf::from("run"),
                PathBuf::from("tmp"),
            ],
            applied_transaction: None,
            route_candidates: Vec::new(),
            origins: BTreeMap::new(),
            deleted_paths: Vec::new(),
            tracked_paths: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteCandidate {
    pub id: String,
    pub destination: String,
    pub interface: String,
    #[serde(default)]
    pub gateway: Option<String>,
    pub ports: Vec<u16>,
    pub created_at: DateTime<Utc>,
    pub observed_route: RouteObservation,
    pub capability_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteObservation {
    pub destination: String,
    pub gateway: Option<String>,
    pub interface: Option<String>,
    #[serde(default)]
    pub ifscope: Option<String>,
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Origin {
    pub existed: bool,
    pub kind: Option<String>,
    pub digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinuxImageChange {
    pub kind: String,
    pub path: PathBuf,
    pub tracked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    TypeChanged,
}

impl std::fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Added => write!(f, "added"),
            Self::Modified => write!(f, "modified"),
            Self::Deleted => write!(f, "deleted"),
            Self::TypeChanged => write!(f, "type_changed"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub path: PathBuf,
    pub kind: ChangeKind,
    pub original_digest: Option<String>,
    pub sandbox_digest: Option<String>,
    pub binary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyPlan {
    pub environment: String,
    pub generated_at: DateTime<Utc>,
    pub changes: Vec<Change>,
    #[serde(default)]
    pub route_candidates: Vec<RouteCandidate>,
    pub lost_required: Vec<String>,
    pub unverifiable_required: Vec<String>,
    pub conflicts: Vec<PathBuf>,
    #[serde(default)]
    pub route_conflicts: Vec<String>,
    #[serde(default)]
    pub guarded_trial: bool,
    #[serde(default)]
    pub deferred_required: Vec<String>,
    pub allowed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    pub path: PathBuf,
    pub existed: bool,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub version: u32,
    pub id: String,
    pub environment: String,
    pub created_at: DateTime<Utc>,
    pub rolled_back_at: Option<DateTime<Utc>>,
    pub base_root: PathBuf,
    pub changes: Vec<Change>,
    pub backups: Vec<BackupEntry>,
    #[serde(default)]
    pub route_changes: Vec<RouteChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouteChangeState {
    Prepared,
    Applying,
    Applied,
    RolledBack,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteChange {
    pub candidate_id: String,
    pub destination: String,
    pub interface: String,
    #[serde(default)]
    pub gateway: Option<String>,
    pub original: RouteObservation,
    pub state: RouteChangeState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_legacy_vm_tracked_paths() {
        let environment = Environment::new(
            "legacy".into(),
            None,
            PathBuf::from("/"),
            EnvironmentPolicy::default(),
            Vec::new(),
        );
        let mut value = serde_json::to_value(environment).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("tracked_paths");
        object.insert(
            "vm_tracked_paths".into(),
            serde_json::json!(["etc/proxy.conf"]),
        );

        let loaded: Environment = serde_json::from_value(value).unwrap();

        assert_eq!(loaded.tracked_paths, vec![PathBuf::from("etc/proxy.conf")]);
    }
}
