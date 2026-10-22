//! B2 store, B4 links and connectedness, B5 churn, B6 layout, and the scheduler.

use std::collections::HashMap;

use crate::channel::EndpointFact;
use crate::contracts::{
    ChurnSummary, Cluster, CoChangePair, Connectedness, EdgeBundle, Heat, Hub, LaneBand, Link, MetricPoint, Thread,
    ViewRef, ViewSpec, WindowDiff, WindowHeader, WindowOutline,
};
use crate::ids::{BlobId, CommitId, LaneId, OpId, RepoPath, Result, ViewId, WindowId, WindowKey};
use crate::lang::BlobFacts;
use crate::vcs::HistoryCommit;

/// World-space rectangle (1 line = 18 wu, 1 column = 9 wu).
#[derive(Debug, Clone, Copy, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// B2 `cv-store`: index.db (derived) + review.db (never deleted by re-indexing).
pub trait Store: Send + Sync {
    fn put_facts(&self, facts: &BlobFacts) -> Result<()>;
    fn facts(&self, blob: &BlobId, pack_version: &str) -> Result<Option<BlobFacts>>;
    fn windows_in_rect(&self, view: &ViewId, rect: Rect) -> Result<Vec<WindowHeader>>;
    fn outlines(&self, view: &ViewId, ids: &[WindowId]) -> Result<Vec<WindowOutline>>;
    fn diffs(&self, view: &ViewId, ids: &[WindowId]) -> Result<Vec<WindowDiff>>;
    fn threads_for_paths(&self, paths: &[RepoPath]) -> Result<Vec<Thread>>;
}

/// Everything one snapshot needs for linking.
pub struct SnapshotFacts {
    pub commit: CommitId,
    pub files: Vec<(RepoPath, BlobId, LaneId)>,
    pub facts: HashMap<BlobId, BlobFacts>,
    pub endpoints: Vec<EndpointFact>,
}

/// B4 `cv-link`.
pub trait Linker: Send + Sync {
    /// Resolves refs to definitions and matches endpoints through channel packs into hubs.
    fn link(&self, snapshot: &SnapshotFacts) -> Result<(Vec<Link>, Vec<Hub>)>;
    /// Dependents within 2 hops (code lane), fan-in/out, test dependents, cycle size; quantile grades.
    fn connectedness(&self, snapshot: &SnapshotFacts, links: &[Link]) -> Result<HashMap<WindowKey, Connectedness>>;
}

pub struct ChurnOptions {
    pub batch: u32,
    pub min_commits: u32,
    pub max_commits: u32,
    pub max_age_days: u32,
    pub half_life_days: f64,
    pub settled_mass: f64,
    pub max_band_drift: f64,
    pub big_commit_files: u32,
    /// Change-mass quantile cut points: Hot, Warm, Mild (defaults 0.5, 0.75, 0.9).
    pub quantiles: [f64; 3],
}

/// B5 `cv-churn`.
pub trait ChurnService: Send + Sync {
    fn grade(
        &self,
        head: &CommitId,
        history: &mut dyn Iterator<Item = Result<HistoryCommit>>,
        options: &ChurnOptions,
    ) -> Result<(ChurnSummary, HashMap<RepoPath, Heat>, Vec<CoChangePair>)>;
}

/// B6 `cv-layout` input: what to show and how it connects, before placement.
pub struct ViewModel {
    pub view: ViewRef,
    pub windows: Vec<WindowHeader>,
    pub links: Vec<Link>,
    pub hubs: Vec<Hub>,
}

pub struct Placement {
    pub layout_version: u64,
    pub world: Rect,
    pub lanes: Vec<LaneBand>,
    pub clusters: Vec<Cluster>,
    pub windows: HashMap<WindowId, Rect>,
    pub hubs: HashMap<String, Rect>,
    pub bundles: Vec<EdgeBundle>,
}

pub enum Layout {
    /// Lanes × heat columns × connectedness rows; dependency clusters whole inside cells.
    Districts,
    /// Dependency clusters only, no axes.
    Dependency,
}

pub trait LayoutService: Send + Sync {
    fn build_view(&self, spec: &ViewSpec, at: &OpId) -> Result<ViewModel>;
    /// Reuses `previous` by WindowKey, honours pins, moves clusters only on settled grade changes.
    fn place(&self, view: &ViewModel, layout: Layout, previous: Option<&Placement>, pins: &[(String, f64, f64)]) -> Result<Placement>;
    fn metrics(&self, view: &ViewId, lane: Option<&LaneId>) -> Result<(Vec<MetricPoint>, Vec<CoChangePair>)>;
}

pub enum IndexJob {
    Sync(OpId),
    Parse { blobs: Vec<BlobId>, visible: bool },
    Link(CommitId),
    Churn(CommitId),
    Layout(ViewId),
    Reanchor(OpId),
}

/// Owned by B7 `cv-server`; visible windows parse first.
pub trait IndexScheduler: Send + Sync {
    fn enqueue(&self, job: IndexJob);
    fn prioritize(&self, view: &ViewId, visible: &[WindowId]);
}
