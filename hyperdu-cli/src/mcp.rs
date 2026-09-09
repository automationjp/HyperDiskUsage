//! The MCP surface: three read-only tools over the HyperDU engine.
//!
//! The tools exist because of what an agent actually has to do when a disk
//! fills up, in order: find out *which* volume is short (`list_volumes`), find
//! out *what* is on it (`scan_path`), and find out *which of that is safe to
//! delete* (`find_reclaimable`). Without the first, a size has no meaning;
//! without the third, the answer is a list the agent cannot act on.
//!
//! There is deliberately no delete tool. Exposing one would create a path for
//! an agent to destroy data without a human in the loop, and the information
//! here is enough to propose a deletion for someone to approve. A wrong report
//! is recoverable; a wrong `rm -rf` is not.

use std::path::PathBuf;

mod progress;

use anyhow::{Context, Result};
use hyperdu_core::volume;
use rmcp::{
    handler::server::wrapper::{Json, Parameters},
    model::{Implementation, ServerCapabilities, ServerInfo},
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::io::stdio,
    ErrorData, RoleServer, ServerHandler, ServiceExt,
};
use serde::{Deserialize, Serialize};

/// Entries returned by `scan_path` when the caller does not say otherwise.
const DEFAULT_TOP_N: usize = 20;
/// Floor for `find_reclaimable`. Below a gigabyte, deleting build output costs
/// more attention than it returns.
const DEFAULT_MIN_RECLAIMABLE_BYTES: u64 = 1 << 30;

const NO_DELETE_NOTE: &str = "Read-only result. This server never deletes anything. \
     Deleting build output is recoverable by rebuilding, but confirm with the user before \
     removing any of these, and prefer a non-zero unused_for_days as a sampled \
     modification-age heuristic; it cannot establish that a build is inactive.";

fn default_top_n() -> usize {
    DEFAULT_TOP_N
}

fn default_min_size() -> u64 {
    DEFAULT_MIN_RECLAIMABLE_BYTES
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScanParams {
    /// Directory to scan. Absolute paths only; a relative path resolves against
    /// the server's working directory, which the caller cannot see.
    pub path: String,
    /// How many of the largest directories to return.
    #[serde(default = "default_top_n")]
    pub top_n: usize,
    /// Stop descending past this depth. 0 means unlimited. Use a small value
    /// on a whole volume: a full scan of a 930 GB disk with 4 million files
    /// takes roughly 47 seconds.
    #[serde(default)]
    pub max_depth: u32,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ScanEntry {
    pub path: String,
    /// Sum of file sizes, matching what `ls` reports.
    pub logical_bytes: u64,
    /// Space actually occupied on disk, which differs from logical for sparse,
    /// compressed, and small files.
    pub physical_bytes: u64,
    pub files: u64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ScanOutput {
    pub root: String,
    /// Largest first, by physical size.
    pub entries: Vec<ScanEntry>,
    /// Directories the scan produced, before `top_n` trimming -- so a caller
    /// can tell "these are all of them" from "these are the biggest 20".
    pub total_directories: usize,
    pub elapsed_ms: u64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct VolumeInfo {
    /// `C:\` on Windows, a mount point on Unix.
    pub mount_point: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub used_bytes: u64,
    /// 0.0 to 1.0. Sort by this to find the volume in trouble.
    pub used_ratio: f64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct VolumesOutput {
    /// Fullest first.
    pub volumes: Vec<VolumeInfo>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReclaimableParams {
    /// Directory to search. Absolute paths only.
    pub path: String,
    /// Ignore candidates smaller than this. Defaults to 1 GiB.
    #[serde(default = "default_min_size")]
    pub min_size_bytes: u64,
    /// Keep only candidates whose sampled modification age reaches this many days.
    /// Omit to list everything. This is a shallow age heuristic and cannot prove
    /// that a build is inactive or make a deletion safe.
    #[serde(default)]
    pub unused_for_days: Option<u64>,
    /// Stop descending past this depth. 0 means unlimited.
    #[serde(default)]
    pub max_depth: u32,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ReclaimableEntry {
    pub path: String,
    /// `rust-target`, `node-modules`, `python-venv`, `python-cache`, or
    /// `gradle-cache`.
    pub kind: String,
    pub size_bytes: u64,
    pub files: u64,
    /// Whole days since the newest file sampled inside. `null` when the
    /// timestamps could not be read, which also fails any `unused_for_days`
    /// filter rather than passing on an unverified guess.
    pub idle_days: Option<u64>,
    /// The command that regenerates this directory, for weighing the cost of
    /// deleting it.
    pub rebuild_hint: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ReclaimableOutput {
    pub root: String,
    /// Largest first.
    pub candidates: Vec<ReclaimableEntry>,
    /// What deleting every listed candidate would free.
    pub total_bytes: u64,
    /// Restated on every response because it governs what the caller may do
    /// with the list: this server never deletes, and an agent should not
    /// either without asking.
    pub note: String,
}

/// Stateless: every tool call re-reads the filesystem, because a cached size is
/// a wrong size the moment a build writes a file.
///
/// `#[tool_handler]` reaches the tools through the `Self::tool_router()`
/// associated function that `#[tool_router]` generates, so there is no router
/// to hold onto here.
#[derive(Clone, Copy, Default)]
pub struct HyperDuServer;

#[tool_router]
impl HyperDuServer {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    #[tool(
        name = "list_volumes",
        description = "List every mounted volume with its total, free, and used bytes, fullest \
                       first. Start here when asked why a disk is full: a directory's size only \
                       means something next to the capacity of the volume holding it."
    )]
    async fn list_volumes(&self) -> Result<Json<VolumesOutput>, ErrorData> {
        // Enumeration touches every drive, and an unresponsive network or
        // optical drive can block; keep it off the protocol thread.
        let mut volumes: Vec<VolumeInfo> = run_blocking(volume::list)
            .await?
            .into_iter()
            .map(|v| VolumeInfo {
                mount_point: v.mount_point.display().to_string(),
                total_bytes: v.total_bytes,
                free_bytes: v.free_bytes,
                used_bytes: v.used_bytes(),
                used_ratio: v.used_ratio(),
            })
            .collect();

        volumes.sort_by(|a, b| {
            b.used_ratio
                .partial_cmp(&a.used_ratio)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(Json(VolumesOutput { volumes }))
    }

    #[tool(
        name = "scan_path",
        description = "Scan a directory tree and return the largest directories inside it, with \
                       logical size, physical size on disk, and file count. Use max_depth to keep \
                       a whole-volume scan bounded; a full 930 GB disk takes about 47 seconds."
    )]
    async fn scan_path(
        &self,
        Parameters(params): Parameters<ScanParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ScanOutput>, ErrorData> {
        self.scan_path_impl(params, Some(context)).await
    }

    async fn scan_path_impl(
        &self,
        params: ScanParams,
        context: Option<RequestContext<RoleServer>>,
    ) -> Result<Json<ScanOutput>, ErrorData> {
        let root = PathBuf::from(&params.path);
        let top_n = params.top_n.max(1);
        let max_depth = params.max_depth;

        let (stats, elapsed_ms) = progress::scan(root.clone(), max_depth, context).await?;

        let total_directories = stats.len();
        // Ranked by core so this agrees with the CLI, including the order equal
        // sizes come out in. Sorting the whole map here to keep `top_n` rows
        // also did more work than the selection core does.
        let entries: Vec<ScanEntry> = hyperdu_core::top_by_physical(stats, top_n)
            .into_iter()
            .map(|(path, stat)| ScanEntry {
                path: path.display().to_string(),
                logical_bytes: stat.logical,
                physical_bytes: stat.physical,
                files: stat.files,
            })
            .collect();

        Ok(Json(ScanOutput {
            root: root.display().to_string(),
            entries,
            total_directories,
            elapsed_ms,
        }))
    }

    #[tool(
        name = "find_reclaimable",
        description = "Find directories holding regenerable build output (Cargo target, \
                       node_modules, virtualenvs, caches) with their size and sampled modification age. \
                       Use this, not scan_path, when the question is what can be rebuilt. Pass \
                       unused_for_days as an age heuristic; it cannot establish that a build is \
                       inactive. This never deletes; it reports."
    )]
    async fn find_reclaimable(
        &self,
        Parameters(params): Parameters<ReclaimableParams>,
    ) -> Result<Json<ReclaimableOutput>, ErrorData> {
        let root = PathBuf::from(&params.path);
        let search_root = root.clone();
        let min_size = params.min_size_bytes;
        let unused_for_days = params.unused_for_days;
        let max_depth = params.max_depth;

        let found = run_blocking(move || {
            hyperdu_core::reclaimable::find(&search_root, min_size, unused_for_days, max_depth)
        })
        .await?;

        let total_bytes = found.iter().map(|c| c.size_bytes).sum();
        let candidates = found
            .into_iter()
            .map(|c| ReclaimableEntry {
                path: c.path.display().to_string(),
                kind: c.kind.as_str().to_owned(),
                size_bytes: c.size_bytes,
                files: c.files,
                idle_days: c.idle_days,
                rebuild_hint: c.kind.rebuild_hint().to_owned(),
            })
            .collect();

        Ok(Json(ReclaimableOutput {
            root: root.display().to_string(),
            candidates,
            total_bytes,
            note: NO_DELETE_NOTE.to_owned(),
        }))
    }
}

/// Run blocking work off the protocol thread, turning a panic in the scanner
/// into a protocol error rather than taking the whole server down with it.
async fn run_blocking<F, T>(f: F) -> Result<T, ErrorData>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ErrorData::internal_error(format!("scan task failed: {e}"), None))
}

#[tool_handler]
impl ServerHandler for HyperDuServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` and `Implementation` are #[non_exhaustive], so rmcp can
        // add fields without breaking us -- at the cost of not being
        // constructible as a literal. Build from the default and fill in.
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "HyperDU exposes disk usage as structured data. To answer \"why is the disk \
             full\", call list_volumes first to find the volume under pressure, then \
             scan_path on it (with max_depth to stay quick) to see what is large, then \
             find_reclaimable to separate what can be rebuilt from what cannot. All three \
             are read-only: nothing here deletes, and deletions should be confirmed with \
             the user before they are run."
                .to_owned(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        // Expanded here rather than via rmcp's `from_build_env`, which would
        // report rmcp's own name and version instead of this crate's.
        info.server_info = Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        info
    }
}

/// Run the MCP transport. This is called only after the CLI has selected the
/// mcp subcommand, so ordinary scans never initialize Tokio or touch stdio.
pub(crate) async fn run() -> Result<()> {
    // stdout carries the protocol. Diagnostics go to stderr, which clients
    // capture as a log; a stray println! here would corrupt the JSON-RPC stream.
    let service = HyperDuServer::new()
        .serve(stdio())
        .await
        .context("failed to start the HyperDU MCP server on stdio")?;

    service
        .waiting()
        .await
        .context("HyperDU MCP server stopped unexpectedly")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_exactly_the_three_documented_tools() {
        // The contract clients actually see. Everything else in this file could
        // be correct while `tools/list` came back empty.
        let mut names: Vec<String> = HyperDuServer::tool_router()
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["find_reclaimable", "list_volumes", "scan_path"]);
    }

    #[test]
    fn every_tool_describes_when_to_reach_for_it() {
        // An agent picks a tool from its description alone; a blank one is a
        // tool that never gets called.
        for tool in HyperDuServer::tool_router().list_all() {
            let description = tool.description.unwrap_or_default();
            assert!(
                description.len() > 40,
                "{} needs a description that says when to use it, got {description:?}",
                tool.name
            );
        }
    }

    #[test]
    fn advertises_tool_support() {
        let info = HyperDuServer::new().get_info();
        assert!(
            info.capabilities.tools.is_some(),
            "a server with no tool capability is invisible to clients"
        );
    }

    #[test]
    fn instructions_state_the_read_only_contract() {
        let info = HyperDuServer::new().get_info();
        let instructions = info.instructions.expect("instructions");
        assert!(
            instructions.contains("read-only"),
            "clients must be told the server does not delete"
        );
    }

    #[test]
    fn reports_the_crate_version_so_clients_can_tell_builds_apart() {
        let info = HyperDuServer::new().get_info();
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.server_info.name, "hyperdu");
    }

    #[tokio::test]
    async fn list_volumes_sorts_the_fullest_first() {
        let out = HyperDuServer::new()
            .list_volumes()
            .await
            .expect("list_volumes");
        let ratios: Vec<f64> = out.0.volumes.iter().map(|v| v.used_ratio).collect();
        assert!(
            ratios.windows(2).all(|w| w[0] >= w[1]),
            "volumes must be ordered fullest first, got {ratios:?}"
        );
    }

    #[tokio::test]
    async fn list_volumes_reports_consistent_totals() {
        let out = HyperDuServer::new()
            .list_volumes()
            .await
            .expect("list_volumes");
        for v in &out.0.volumes {
            assert!(
                v.free_bytes <= v.total_bytes,
                "{} reports more free than total",
                v.mount_point
            );
            assert_eq!(v.used_bytes, v.total_bytes - v.free_bytes);
        }
    }

    #[tokio::test]
    async fn scan_path_returns_the_largest_first_and_honours_top_n() {
        let dir = tempfile::tempdir().expect("temp dir");
        for (name, size) in [("big", 4096), ("mid", 2048), ("small", 16)] {
            let sub = dir.path().join(name);
            std::fs::create_dir_all(&sub).expect("create dir");
            std::fs::write(sub.join("data.bin"), vec![0u8; size]).expect("write");
        }

        let out = HyperDuServer::new()
            .scan_path_impl(
                ScanParams {
                    path: dir.path().display().to_string(),
                    top_n: 2,
                    max_depth: 0,
                },
                None,
            )
            .await
            .expect("scan_path");

        assert_eq!(out.0.entries.len(), 2, "top_n must trim the result");
        assert!(
            out.0.entries[0].physical_bytes >= out.0.entries[1].physical_bytes,
            "entries must be ordered largest first"
        );
        assert!(
            out.0.total_directories >= out.0.entries.len(),
            "the untrimmed count must not be smaller than what was returned"
        );
    }

    #[tokio::test]
    async fn scan_path_reports_an_error_for_a_missing_directory() {
        let out = HyperDuServer::new()
            .scan_path_impl(
                ScanParams {
                    path: "/hyperdu-nonexistent-scan-probe-71ac".to_owned(),
                    top_n: 5,
                    max_depth: 0,
                },
                None,
            )
            .await;
        assert!(
            out.is_err(),
            "a missing path must surface as an error, not an empty success"
        );
    }

    #[tokio::test]
    async fn scan_path_treats_zero_top_n_as_one_rather_than_returning_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("f.bin"), vec![0u8; 512]).expect("write");

        let out = HyperDuServer::new()
            .scan_path_impl(
                ScanParams {
                    path: dir.path().display().to_string(),
                    top_n: 0,
                    max_depth: 0,
                },
                None,
            )
            .await
            .expect("scan_path");
        assert_eq!(out.0.entries.len(), 1);
    }

    #[tokio::test]
    async fn find_reclaimable_finds_build_output_and_carries_the_no_delete_note() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]").expect("write manifest");
        let target = dir.path().join("target/debug");
        std::fs::create_dir_all(&target).expect("create target");
        std::fs::write(target.join("app.o"), vec![0u8; 4096]).expect("write artifact");

        let out = HyperDuServer::new()
            .find_reclaimable(Parameters(ReclaimableParams {
                path: dir.path().display().to_string(),
                min_size_bytes: 0,
                unused_for_days: None,
                max_depth: 0,
            }))
            .await
            .expect("find_reclaimable");

        assert_eq!(out.0.candidates.len(), 1);
        assert_eq!(out.0.candidates[0].kind, "rust-target");
        assert_eq!(out.0.candidates[0].rebuild_hint, "cargo build");
        assert!(out.0.total_bytes > 0);
        assert!(
            out.0.note.contains("never deletes"),
            "every response must restate that this server does not delete"
        );
    }

    #[tokio::test]
    async fn find_reclaimable_excludes_freshly_written_output() {
        // Fresh sampled output is excluded, but age alone cannot prove a build is inactive.
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]").expect("write manifest");
        let target = dir.path().join("target");
        std::fs::create_dir_all(&target).expect("create target");
        std::fs::write(target.join("fresh.o"), vec![0u8; 4096]).expect("write artifact");

        let out = HyperDuServer::new()
            .find_reclaimable(Parameters(ReclaimableParams {
                path: dir.path().display().to_string(),
                min_size_bytes: 0,
                unused_for_days: Some(1),
                max_depth: 0,
            }))
            .await
            .expect("find_reclaimable");

        assert!(
            out.0.candidates.is_empty(),
            "a directory written to seconds ago must not be offered for deletion"
        );
        assert_eq!(out.0.total_bytes, 0);
    }

    #[tokio::test]
    async fn find_reclaimable_returns_an_empty_list_for_a_missing_root() {
        // Unlike scan_path, "nothing to reclaim here" is a useful answer rather
        // than an error: the caller asked what could be freed, not what exists.
        let out = HyperDuServer::new()
            .find_reclaimable(Parameters(ReclaimableParams {
                path: "/hyperdu-nonexistent-reclaim-probe-3ba9".to_owned(),
                min_size_bytes: 0,
                unused_for_days: None,
                max_depth: 0,
            }))
            .await
            .expect("find_reclaimable");
        assert!(out.0.candidates.is_empty());
    }
}
