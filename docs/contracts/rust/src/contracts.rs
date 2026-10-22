//! Stand-ins for the wire types that `cv-contracts` defines from docs/contracts/model.ts.
//! The real crate derives serde, ts-rs (TypeScript for the web client) and schemars (MCP schemas).

macro_rules! wire_type {
    ($($name:ident),* $(,)?) => {$(
        #[doc = concat!("See `", stringify!($name), "` in docs/contracts/*.ts.")]
        #[derive(Debug, Clone, Default)]
        pub struct $name;
    )*};
}

wire_type! {
    Revision, FileDiff, ConflictRecord, ConflictDetail,
    Link, Hub, Heat, Connectedness, ChurnSummary,
    Thread, NewThread, EditRequest, EditResult,
    ViewSpec, ViewRef, WindowHeader, WindowOutline, WindowDiff, Cluster, EdgeBundle, LaneBand,
    ServerEvent, WorkspaceSummary, MetricPoint, CoChangePair, LaneDef, Actor,
}
