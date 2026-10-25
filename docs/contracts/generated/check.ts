// Generated from crates/cv-contracts by scripts/gen-contracts.sh (ts-rs). Do not edit.
// The hand-written spec is docs/contracts/*.ts; check.ts compares the two.
// Compile-time comparison of the generated types (G) with the hand-written spec (S_*). Levels come
// from crates/cv-contracts/ts-divergences.txt: exact = mutually assignable (default); out = Rust's
// output conforms to the spec; in = the spec's input is accepted by Rust; keys = same field names
// and required fields. *Keys assertions also require the same top-level field names.

import type * as G from "./index";
import type * as S_http from "../http";
import type * as S_ids from "../ids";
import type * as S_model from "../model";
import type * as S_ws from "../ws";

export type Assert<T extends true> = T;
export type Extends<A, B> = [A] extends [B] ? true : false;
export type Exact<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;
// U<T> compares shapes, not refinements: at any depth it drops the compile-time id brands of
// ids.ts, widens literal unions to string / number / boolean (Rust carries those as String,
// integers and bool, validated at runtime), and treats fields Rust keeps as untyped JSON
// (serde_json::Value) as matching anything.
export type U<T> = Exact<T, G.JsonValue> extends true ? any : UD<T>;
type UD<T> = T extends string
  ? string
  : T extends number
    ? number
    : T extends boolean
      ? boolean
      : T extends object
        ? { [K in keyof T]: U<T[K]> }
        : T;
export type AllKeys<T> = T extends unknown ? keyof T : never;
type RequiredKeys<T> = T extends unknown
  ? { [K in keyof T]-?: object extends Pick<T, K> ? never : K }[keyof T]
  : never;
export type SameKeys<A, B> =
  Exact<AllKeys<A>, AllKeys<B>> extends true ? Exact<RequiredKeys<A>, RequiredKeys<B>> : false;

export type _Rect = Assert<Exact<U<G.Rect>, U<S_model.Rect>>>;
export type _RectKeys = Assert<Exact<AllKeys<G.Rect>, AllKeys<S_model.Rect>>>;
export type _LaneDef = Assert<Exact<U<G.LaneDef>, U<S_model.LaneDef>>>;
export type _LaneDefKeys = Assert<Exact<AllKeys<G.LaneDef>, AllKeys<S_model.LaneDef>>>;
export type _LaneBand = Assert<Exact<U<G.LaneBand>, U<S_model.LaneBand>>>;
export type _LaneBandKeys = Assert<Exact<AllKeys<G.LaneBand>, AllKeys<S_model.LaneBand>>>;
export type _GridColumn = Assert<Exact<U<G.GridColumn>, U<S_model.GridColumn>>>;
export type _GridColumnKeys = Assert<Exact<AllKeys<G.GridColumn>, AllKeys<S_model.GridColumn>>>;
export type _Cluster = Assert<Extends<U<G.Cluster>, U<S_model.Cluster>>>;
export type _ClusterKeys = Assert<Exact<AllKeys<G.Cluster>, AllKeys<S_model.Cluster>>>;
export type _FolderFrame = Assert<Exact<U<G.FolderFrame>, U<S_model.FolderFrame>>>;
export type _FolderFrameKeys = Assert<Exact<AllKeys<G.FolderFrame>, AllKeys<S_model.FolderFrame>>>;
export type _SiteRef = Assert<Exact<U<G.SiteRef>, U<S_model.SiteRef>>>;
export type _SiteRefKeys = Assert<Exact<AllKeys<G.SiteRef>, AllKeys<S_model.SiteRef>>>;
export type _Hub = Assert<Exact<U<G.Hub>, U<S_model.Hub>>>;
export type _HubKeys = Assert<Exact<AllKeys<G.Hub>, AllKeys<S_model.Hub>>>;
export type _EdgeBundle = Assert<Exact<U<G.EdgeBundle>, U<S_model.EdgeBundle>>>;
export type _EdgeBundleKeys = Assert<Exact<AllKeys<G.EdgeBundle>, AllKeys<S_model.EdgeBundle>>>;
export type _ViewRef = Assert<Exact<U<G.ViewRef>, U<S_model.ViewRef>>>;
export type _ViewRefKeys = Assert<Exact<AllKeys<G.ViewRef>, AllKeys<S_model.ViewRef>>>;
export type _ChurnSummary = Assert<Exact<U<G.ChurnSummary>, U<S_model.ChurnSummary>>>;
export type _ChurnSummaryKeys = Assert<Exact<AllKeys<G.ChurnSummary>, AllKeys<S_model.ChurnSummary>>>;
export type _ViewBootstrap = Assert<Extends<U<G.ViewBootstrap>, U<S_http.ViewBootstrap>>>;
export type _ViewBootstrapKeys = Assert<Exact<AllKeys<G.ViewBootstrap>, AllKeys<S_http.ViewBootstrap>>>;
export type _Heat = Assert<Exact<U<G.Heat>, U<S_model.Heat>>>;
export type _HeatKeys = Assert<Exact<AllKeys<G.Heat>, AllKeys<S_model.Heat>>>;
export type _Connectedness = Assert<Exact<U<G.Connectedness>, U<S_model.Connectedness>>>;
export type _ConnectednessKeys = Assert<Exact<AllKeys<G.Connectedness>, AllKeys<S_model.Connectedness>>>;
export type _FileFragment = Assert<Exact<U<G.FileFragment>, U<S_model.FileFragment>>>;
export type _FileFragmentKeys = Assert<Exact<AllKeys<G.FileFragment>, AllKeys<S_model.FileFragment>>>;
export type _WindowHeader = Assert<Exact<U<G.WindowHeader>, U<S_model.WindowHeader>>>;
export type _WindowHeaderKeys = Assert<Exact<AllKeys<G.WindowHeader>, AllKeys<S_model.WindowHeader>>>;
export type _TileEdge = Assert<Exact<U<G.TileEdge>, U<S_http.TileEdge>>>;
export type _TileEdgeKeys = Assert<Exact<AllKeys<G.TileEdge>, AllKeys<S_http.TileEdge>>>;
export type _TileResponse = Assert<Exact<U<G.TileResponse>, U<S_http.TileResponse>>>;
export type _TileResponseKeys = Assert<Exact<AllKeys<G.TileResponse>, AllKeys<S_http.TileResponse>>>;
export type _WindowBatchRequest = Assert<Exact<U<G.WindowBatchRequest>, U<S_http.WindowBatchRequest>>>;
export type _WindowBatchRequestKeys = Assert<Exact<AllKeys<G.WindowBatchRequest>, AllKeys<S_http.WindowBatchRequest>>>;
export type _Port = Assert<Exact<U<G.Port>, U<S_model.Port>>>;
export type _PortKeys = Assert<Exact<AllKeys<G.Port>, AllKeys<S_model.Port>>>;
export type _WindowOutline = Assert<Exact<U<G.WindowOutline>, U<S_model.WindowOutline>>>;
export type _WindowOutlineKeys = Assert<Exact<AllKeys<G.WindowOutline>, AllKeys<S_model.WindowOutline>>>;
export type _Link = Assert<Exact<U<G.Link>, U<S_model.Link>>>;
export type _LinkKeys = Assert<Exact<AllKeys<G.Link>, AllKeys<S_model.Link>>>;
export type _WindowBatchResponse = Assert<Exact<U<G.WindowBatchResponse>, U<S_http.WindowBatchResponse>>>;
export type _WindowBatchResponseKeys = Assert<Exact<AllKeys<G.WindowBatchResponse>, AllKeys<S_http.WindowBatchResponse>>>;
export type _BlobText = Assert<Exact<U<G.BlobText>, U<S_http.BlobText>>>;
export type _BlobTextKeys = Assert<Exact<AllKeys<G.BlobText>, AllKeys<S_http.BlobText>>>;
export type _RevisionFlags = Assert<Exact<U<G.RevisionFlags>, U<S_model.RevisionFlags>>>;
export type _RevisionFlagsKeys = Assert<Exact<AllKeys<G.RevisionFlags>, AllKeys<S_model.RevisionFlags>>>;
export type _Revision = Assert<Exact<U<G.Revision>, U<S_model.Revision>>>;
export type _RevisionKeys = Assert<Exact<AllKeys<G.Revision>, AllKeys<S_model.Revision>>>;
export type _RevisionReview = Assert<Exact<U<G.RevisionReview>, U<S_model.RevisionReview>>>;
export type _RevisionReviewKeys = Assert<Exact<AllKeys<G.RevisionReview>, AllKeys<S_model.RevisionReview>>>;
export type _GraphRow = Assert<Exact<U<G.GraphRow>, U<S_model.GraphRow>>>;
export type _GraphRowKeys = Assert<Exact<AllKeys<G.GraphRow>, AllKeys<S_model.GraphRow>>>;
export type _WorkspaceSummary = Assert<Extends<U<G.WorkspaceSummary>, U<S_model.WorkspaceSummary>>>;
export type _WorkspaceSummaryKeys = Assert<Exact<AllKeys<G.WorkspaceSummary>, AllKeys<S_model.WorkspaceSummary>>>;
export type _RangeWarning = Assert<Exact<U<G.RangeWarning>, U<S_model.RangeWarning>>>;
export type _RangeWarningKeys = Assert<Exact<AllKeys<G.RangeWarning>, AllKeys<S_model.RangeWarning>>>;
export type _RevsetValidation = Assert<Exact<U<G.RevsetValidation>, U<S_http.RevsetValidation>>>;
export type _RevsetValidationKeys = Assert<Exact<AllKeys<G.RevsetValidation>, AllKeys<S_http.RevsetValidation>>>;
export type _RangeOptions = Assert<Exact<U<G.RangeOptions>, U<S_model.RangeOptions>>>;
export type _RangeOptionsKeys = Assert<Exact<AllKeys<G.RangeOptions>, AllKeys<S_model.RangeOptions>>>;
export type _SavedRange = Assert<Exact<U<G.SavedRange>, U<S_http.SavedRange>>>;
export type _SavedRangeKeys = Assert<Exact<AllKeys<G.SavedRange>, AllKeys<S_http.SavedRange>>>;
export type _DiffLine = Assert<Exact<U<G.DiffLine>, U<S_model.DiffLine>>>;
export type _DiffLineKeys = Assert<Exact<AllKeys<G.DiffLine>, AllKeys<S_model.DiffLine>>>;
export type _WindowDiff = Assert<Exact<U<G.WindowDiff>, U<S_model.WindowDiff>>>;
export type _WindowDiffKeys = Assert<Exact<AllKeys<G.WindowDiff>, AllKeys<S_model.WindowDiff>>>;
export type _MetricPoint = Assert<Exact<U<G.MetricPoint>, U<S_model.MetricPoint>>>;
export type _MetricPointKeys = Assert<Exact<AllKeys<G.MetricPoint>, AllKeys<S_model.MetricPoint>>>;
export type _CoChangePair = Assert<Exact<U<G.CoChangePair>, U<S_model.CoChangePair>>>;
export type _CoChangePairKeys = Assert<Exact<AllKeys<G.CoChangePair>, AllKeys<S_model.CoChangePair>>>;
export type _CoChangeClusterPair = Assert<Exact<U<G.CoChangeClusterPair>, U<S_model.CoChangeClusterPair>>>;
export type _CoChangeClusterPairKeys = Assert<Exact<AllKeys<G.CoChangeClusterPair>, AllKeys<S_model.CoChangeClusterPair>>>;
export type _StructureResponse = Assert<Exact<U<G.StructureResponse>, U<S_model.StructureResponse>>>;
export type _StructureResponseKeys = Assert<Exact<AllKeys<G.StructureResponse>, AllKeys<S_model.StructureResponse>>>;
export type _StructureSummary = Assert<Exact<U<G.StructureSummary>, U<S_model.StructureSummary>>>;
export type _StructureSummaryKeys = Assert<Exact<AllKeys<G.StructureSummary>, AllKeys<S_model.StructureSummary>>>;
export type _FolderStructure = Assert<Exact<U<G.FolderStructure>, U<S_model.FolderStructure>>>;
export type _FolderStructureKeys = Assert<Exact<AllKeys<G.FolderStructure>, AllKeys<S_model.FolderStructure>>>;
export type _FileStructure = Assert<Exact<U<G.FileStructure>, U<S_model.FileStructure>>>;
export type _FileStructureKeys = Assert<Exact<AllKeys<G.FileStructure>, AllKeys<S_model.FileStructure>>>;
export type _WindowStructure = Assert<Exact<U<G.WindowStructure>, U<S_model.WindowStructure>>>;
export type _WindowStructureKeys = Assert<Exact<AllKeys<G.WindowStructure>, AllKeys<S_model.WindowStructure>>>;
export type _SearchHit = Assert<Exact<U<G.SearchHit>, U<S_http.SearchHit>>>;
export type _SearchHitKeys = Assert<Exact<AllKeys<G.SearchHit>, AllKeys<S_http.SearchHit>>>;
export type _ExpandRequest = Assert<Extends<U<S_http.ExpandRequest>, U<G.ExpandRequest>>>;
export type _ExpandRequestKeys = Assert<Exact<AllKeys<G.ExpandRequest>, AllKeys<S_http.ExpandRequest>>>;
export type _Actor = Assert<Exact<U<G.Actor>, U<S_ids.Actor>>>;
export type _ActorKeys = Assert<Exact<AllKeys<G.Actor>, AllKeys<S_ids.Actor>>>;
export type _Anchor = Assert<Exact<U<G.Anchor>, U<S_model.Anchor>>>;
export type _AnchorKeys = Assert<Exact<AllKeys<G.Anchor>, AllKeys<S_model.Anchor>>>;
export type _Comment = Assert<Exact<U<G.Comment>, U<S_model.Comment>>>;
export type _CommentKeys = Assert<Exact<AllKeys<G.Comment>, AllKeys<S_model.Comment>>>;
export type _Thread = Assert<Exact<U<G.Thread>, U<S_model.Thread>>>;
export type _ThreadKeys = Assert<Exact<AllKeys<G.Thread>, AllKeys<S_model.Thread>>>;
export type _NewThread = Assert<Extends<U<S_http.NewThread>, U<G.NewThread>>>;
export type _NewThreadKeys = Assert<Exact<AllKeys<G.NewThread>, AllKeys<S_http.NewThread>>>;
export type _LinePatch = Assert<Exact<U<G.LinePatch>, U<S_model.LinePatch>>>;
export type _LinePatchKeys = Assert<Exact<AllKeys<G.LinePatch>, AllKeys<S_model.LinePatch>>>;
export type _EditRequest = Assert<Exact<U<G.EditRequest>, U<S_model.EditRequest>>>;
export type _EditRequestKeys = Assert<Exact<AllKeys<G.EditRequest>, AllKeys<S_model.EditRequest>>>;
export type _EditResult = Assert<Exact<U<G.EditResult>, U<S_model.EditResult>>>;
export type _EditResultKeys = Assert<Exact<AllKeys<G.EditResult>, AllKeys<S_model.EditResult>>>;
export type _EditWarning = Assert<Exact<U<G.EditWarning>, U<S_model.EditWarning>>>;
export type _EditWarningKeys = Assert<Exact<AllKeys<G.EditWarning>, AllKeys<S_model.EditWarning>>>;
export type _UndoBlocked = Assert<Exact<U<G.UndoBlocked>, U<S_model.UndoBlocked>>>;
export type _UndoBlockedKeys = Assert<Exact<AllKeys<G.UndoBlocked>, AllKeys<S_model.UndoBlocked>>>;
export type _BookmarkInfo = Assert<Exact<U<G.BookmarkInfo>, U<S_model.BookmarkInfo>>>;
export type _BookmarkInfoKeys = Assert<Exact<AllKeys<G.BookmarkInfo>, AllKeys<S_model.BookmarkInfo>>>;
export type _LandSource = Assert<Exact<U<G.LandSource>, U<S_model.LandSource>>>;
export type _LandSourceKeys = Assert<Exact<AllKeys<G.LandSource>, AllKeys<S_model.LandSource>>>;
export type _GraphOp = Assert<Exact<U<G.GraphOp>, U<S_model.GraphOp>>>;
export type _GraphOpKeys = Assert<Exact<AllKeys<G.GraphOp>, AllKeys<S_model.GraphOp>>>;
export type _GraphOpRequest = Assert<Exact<U<G.GraphOpRequest>, U<S_model.GraphOpRequest>>>;
export type _GraphOpRequestKeys = Assert<Exact<AllKeys<G.GraphOpRequest>, AllKeys<S_model.GraphOpRequest>>>;
export type _GraphOpResult = Assert<Exact<U<G.GraphOpResult>, U<S_model.GraphOpResult>>>;
export type _GraphOpResultKeys = Assert<Exact<AllKeys<G.GraphOpResult>, AllKeys<S_model.GraphOpResult>>>;
export type _OpEntry = Assert<Exact<U<G.OpEntry>, U<S_model.OpEntry>>>;
export type _OpEntryKeys = Assert<Exact<AllKeys<G.OpEntry>, AllKeys<S_model.OpEntry>>>;
export type _OpLog = Assert<Exact<U<G.OpLog>, U<S_model.OpLog>>>;
export type _OpLogKeys = Assert<Exact<AllKeys<G.OpLog>, AllKeys<S_model.OpLog>>>;
export type _OpDetail = Assert<Exact<U<G.OpDetail>, U<S_model.OpDetail>>>;
export type _OpDetailKeys = Assert<Exact<AllKeys<G.OpDetail>, AllKeys<S_model.OpDetail>>>;
export type _RestoreRequest = Assert<Exact<U<G.RestoreRequest>, U<S_model.RestoreRequest>>>;
export type _RestoreRequestKeys = Assert<Exact<AllKeys<G.RestoreRequest>, AllKeys<S_model.RestoreRequest>>>;
export type _FetchRequest = Assert<Exact<U<G.FetchRequest>, U<S_model.FetchRequest>>>;
export type _FetchRequestKeys = Assert<Exact<AllKeys<G.FetchRequest>, AllKeys<S_model.FetchRequest>>>;
export type _PushRequest = Assert<Exact<U<G.PushRequest>, U<S_model.PushRequest>>>;
export type _PushRequestKeys = Assert<Exact<AllKeys<G.PushRequest>, AllKeys<S_model.PushRequest>>>;
export type _RemoteResult = Assert<Exact<U<G.RemoteResult>, U<S_model.RemoteResult>>>;
export type _RemoteResultKeys = Assert<Exact<AllKeys<G.RemoteResult>, AllKeys<S_model.RemoteResult>>>;
export type _ConflictRecord = Assert<Exact<U<G.ConflictRecord>, U<S_model.ConflictRecord>>>;
export type _ConflictRecordKeys = Assert<Exact<AllKeys<G.ConflictRecord>, AllKeys<S_model.ConflictRecord>>>;
export type _ConflictDetail = Assert<Exact<U<G.ConflictDetail>, U<S_model.ConflictDetail>>>;
export type _ConflictDetailKeys = Assert<Exact<AllKeys<G.ConflictDetail>, AllKeys<S_model.ConflictDetail>>>;
export type _ServerEvent = Assert<Exact<U<G.ServerEvent>, U<S_ws.ServerEvent>>>;
export type _ServerEventKeys = Assert<Exact<AllKeys<G.ServerEvent>, AllKeys<S_ws.ServerEvent>>>;
export type _ServerFrame = Assert<Exact<U<G.ServerFrame>, U<S_ws.ServerFrame>>>;
export type _ServerFrameKeys = Assert<Exact<AllKeys<G.ServerFrame>, AllKeys<S_ws.ServerFrame>>>;
export type _ClientMessage = Assert<Exact<U<G.ClientMessage>, U<S_ws.ClientMessage>>>;
export type _ClientMessageKeys = Assert<Exact<AllKeys<G.ClientMessage>, AllKeys<S_ws.ClientMessage>>>;
export type _ApiError = Assert<Exact<U<G.ApiError>, U<S_ids.ApiError>>>;
export type _ApiErrorKeys = Assert<Exact<AllKeys<G.ApiError>, AllKeys<S_ids.ApiError>>>;
export type _WorkspaceInfo = Assert<Exact<U<G.WorkspaceInfo>, U<S_model.WorkspaceInfo>>>;
export type _WorkspaceInfoKeys = Assert<Exact<AllKeys<G.WorkspaceInfo>, AllKeys<S_model.WorkspaceInfo>>>;
export type _SnapshotOptions = Assert<Exact<U<G.SnapshotOptions>, U<S_model.SnapshotOptions>>>;
export type _SnapshotOptionsKeys = Assert<Exact<AllKeys<G.SnapshotOptions>, AllKeys<S_model.SnapshotOptions>>>;
export type _ViewSpec = Assert<Exact<U<G.ViewSpec>, U<S_model.ViewSpec>>>;
export type _ViewSpecKeys = Assert<Exact<AllKeys<G.ViewSpec>, AllKeys<S_model.ViewSpec>>>;
export type _Viewport = Assert<Exact<U<G.Viewport>, U<S_model.Viewport>>>;
export type _ViewportKeys = Assert<Exact<AllKeys<G.Viewport>, AllKeys<S_model.Viewport>>>;
export type _FileDiff = Assert<Exact<U<G.FileDiff>, U<S_model.FileDiff>>>;
export type _FileDiffKeys = Assert<Exact<AllKeys<G.FileDiff>, AllKeys<S_model.FileDiff>>>;
export type _ThreadSummary = Assert<Exact<U<G.ThreadSummary>, U<S_model.ThreadSummary>>>;
export type _ThreadSummaryKeys = Assert<Exact<AllKeys<G.ThreadSummary>, AllKeys<S_model.ThreadSummary>>>;
export type _ViewState = Assert<Exact<U<G.ViewState>, U<S_ws.ViewState>>>;
export type _ViewStateKeys = Assert<Exact<AllKeys<G.ViewState>, AllKeys<S_ws.ViewState>>>;
export type _SetupInfo = Assert<Exact<U<G.SetupInfo>, U<S_http.SetupInfo>>>;
export type _SetupInfoKeys = Assert<Exact<AllKeys<G.SetupInfo>, AllKeys<S_http.SetupInfo>>>;
export type _GitCommitDetail = Assert<Exact<U<G.GitCommitDetail>, U<S_http.GitCommitDetail>>>;
export type _GitCommitDetailKeys = Assert<Exact<AllKeys<G.GitCommitDetail>, AllKeys<S_http.GitCommitDetail>>>;
export type _RepoInfo = Assert<Exact<U<G.RepoInfo>, U<S_http.RepoInfo>>>;
export type _RepoInfoKeys = Assert<Exact<AllKeys<G.RepoInfo>, AllKeys<S_http.RepoInfo>>>;
export type _TourStep = Assert<Exact<U<G.TourStep>, U<S_http.TourStep>>>;
export type _TourStepKeys = Assert<Exact<AllKeys<G.TourStep>, AllKeys<S_http.TourStep>>>;
export type _Tour = Assert<Exact<U<G.Tour>, U<S_http.Tour>>>;
export type _TourKeys = Assert<Exact<AllKeys<G.Tour>, AllKeys<S_http.Tour>>>;
export type _AgentPermissions = Assert<Exact<U<G.AgentPermissions>, U<S_http.AgentPermissions>>>;
export type _AgentPermissionsKeys = Assert<Exact<AllKeys<G.AgentPermissions>, AllKeys<S_http.AgentPermissions>>>;
export type _FragmentsResponse = Assert<Exact<U<G.FragmentsResponse>, U<S_http.FragmentsResponse>>>;
export type _FragmentsResponseKeys = Assert<Exact<AllKeys<G.FragmentsResponse>, AllKeys<S_http.FragmentsResponse>>>;
export type _LayoutPatch = Assert<Exact<U<G.LayoutPatch>, U<S_http.LayoutPatch>>>;
export type _LayoutPatchKeys = Assert<Exact<AllKeys<G.LayoutPatch>, AllKeys<S_http.LayoutPatch>>>;

// Rust-only (no spec namesake): JsonValue, Texture, LaneRow, ViewStats, Resolved, InRange, SymbolRef, LineSpan, LineRange, WindowCounts, WindowFlags, DiffStat, Heading, Signature, ShortestPrefix, GraphEdge, RevisionsResponse, WorkspaceWc, StackSummary, AgentBinding, OpRef, WorkspaceOverlap, StaleReason, WorkspaceStale, RevsetSummary, RevsetError, MetricsResponse, FolderPull, SearchTarget, ExpandAmount, Fingerprint, Outdated, NewAnchor, EditFile, EditOrigin, EditWorkspace, EditError, EditedFile, LaterOp, BookmarkRemote, RefsResponse, BookmarkMove, ConflictSide, ConflictRegion, EventsResponse, ViewSelection, FileHunk, RepoIndex, RepoCapabilities, JjInstall, GitFileStat, FragmentRef, LayoutMove, InterdiffResponse, AgentViewState
