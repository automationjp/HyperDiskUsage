//! HyperDU as an MCP server over stdio.
//!
//! Agents already had the engine available through the CLI, but only as text:
//! reaching an answer meant six shell round-trips and a chain of `sed` and
//! `awk` that broke whenever the output format shifted. This speaks the Model
//! Context Protocol instead, so the same questions are one typed call with a
//! structured answer.
//!
//! The three layers HyperDU offers agents are deliberately independent. This
//! binary needs neither the Agent Skill nor the plugin bundle in `plugin/` to
//! work, and the skill drives the CLI rather than this server, so either can be
//! adopted without the other.
//!
//! Run it directly to try it; MCP clients launch it themselves:
//!
//! ```text
//! hyperdu-mcp
//! ```

use anyhow::{Context, Result};
use rmcp::{transport::io::stdio, ServiceExt};

mod reclaimable;
mod server;

#[tokio::main]
async fn main() -> Result<()> {
    // stdout carries the protocol. Diagnostics go to stderr, which clients
    // capture as a log; a stray `println!` anywhere in this crate would corrupt
    // the JSON-RPC stream and look like a client bug.
    let service = server::HyperDuServer::new()
        .serve(stdio())
        .await
        .context("failed to start the HyperDU MCP server on stdio")?;

    // Resolves when the client disconnects, which is the normal way to exit.
    service
        .waiting()
        .await
        .context("HyperDU MCP server stopped unexpectedly")?;
    Ok(())
}
