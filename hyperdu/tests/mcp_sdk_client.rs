//! Official MCP SDK client over a real `hyperdu mcp` child process.
//! Each protocol wait is bounded; dropping a failed test kills its own server.
use std::{future::Future, path::Path, process::Stdio, time::Duration};

use rmcp::{
    model::{
        CallToolRequest, CallToolRequestParams, ClientRequest, ProgressNotificationParam,
        ProgressToken, ServerResult,
    },
    service::{NotificationContext, PeerRequestOptions, RequestHandle, RunningService},
    ClientHandler, RoleClient, ServiceExt,
};
use serde_json::json;
use tokio::{
    process::{Child, Command},
    sync::mpsc,
};

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), future)
        .await
        .expect("MCP operation exceeded 15 seconds")
}
#[derive(Clone)]
struct ProgressClient(mpsc::UnboundedSender<ProgressNotificationParam>);
impl ClientHandler for ProgressClient {
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _: NotificationContext<RoleClient>,
    ) {
        let _ = self.0.send(params);
    }
}
struct Sdk {
    service: RunningService<RoleClient, ProgressClient>,
    child: Child,
    progress: mpsc::UnboundedReceiver<ProgressNotificationParam>,
}
impl Sdk {
    async fn start() -> anyhow::Result<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_hyperdu"))
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let (tx, progress) = mpsc::unbounded_channel();
        let transport = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
        let service = bounded(ProgressClient(tx).serve(transport)).await?;
        Ok(Self {
            service,
            child,
            progress,
        })
    }
    async fn scan(&self, path: &Path) -> anyhow::Result<RequestHandle<RoleClient>> {
        let params = CallToolRequestParams::new("scan_path").with_arguments(
            json!({"path":path.display().to_string(),"top_n":1,"max_depth":0})
                .as_object()
                .unwrap()
                .clone(),
        );
        Ok(bounded(self.service.send_request_with_option(
            ClientRequest::CallToolRequest(CallToolRequest::new(params)),
            PeerRequestOptions::default(),
        ))
        .await?)
    }
    async fn stop(mut self) -> anyhow::Result<()> {
        bounded(self.service.cancel()).await?;
        let status = bounded(self.child.wait()).await?;
        anyhow::ensure!(status.success(), "MCP process exited {status}");
        Ok(())
    }
}
fn fixture(files: usize, bytes: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for n in 0..files {
        std::fs::write(dir.path().join(format!("{n}.bin")), vec![0u8; bytes]).unwrap();
    }
    dir
}
fn assert_result(result: ServerResult, path: &Path, files: u64, bytes: u64) {
    let ServerResult::CallToolResult(result) = result else {
        panic!("not a tool result")
    };
    assert_ne!(result.is_error, Some(true));
    let structured = result
        .structured_content
        .expect("missing structured result");
    assert_eq!(structured["entries"][0]["path"], path.display().to_string());
    assert_eq!(structured["entries"][0]["files"], files);
    assert_eq!(structured["entries"][0]["logical_bytes"], bytes);
}

#[tokio::test]
async fn sdk_concurrent_scans_keep_progress_tokens_and_results_separate() -> anyhow::Result<()> {
    let a = fixture(1024, 3);
    let b = fixture(1, 19);
    let mut sdk = Sdk::start().await?;
    let tools = bounded(sdk.service.list_all_tools()).await?;
    assert!(tools.iter().any(|tool| tool.name == "scan_path"));
    let first = sdk.scan(a.path()).await?;
    let second = sdk.scan(b.path()).await?;
    let tokens = [first.progress_token.clone(), second.progress_token.clone()];
    assert_ne!(tokens[0], tokens[1]);
    // Both requests are in flight before either result is awaited.
    let (first, second) = tokio::join!(
        bounded(first.await_response()),
        bounded(second.await_response())
    );
    assert_result(first?, a.path(), 1024, 3072);
    assert_result(second?, b.path(), 1, 19);
    let mut last = [None, None];
    while last != [Some(1024.0), Some(1.0)] {
        let event = bounded(sdk.progress.recv())
            .await
            .expect("progress stream closed");
        let index = tokens
            .iter()
            .position(|token| *token == event.progress_token)
            .expect("unexpected request token");
        assert!(event.total.is_none(), "unknown total must stay absent");
        if let Some(previous) = last[index] {
            assert!(event.progress > previous);
        } else {
            assert_eq!(event.progress, 0.0);
            assert!(event
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("Scanning"));
        }
        last[index] = Some(event.progress);
    }
    sdk.stop().await
}

#[tokio::test]
async fn sdk_rejected_arguments_do_not_break_the_next_scan() -> anyhow::Result<()> {
    let dir = fixture(1, 7);
    let sdk = Sdk::start().await?;
    let bad = CallToolRequestParams::new("scan_path").with_arguments(
        json!({"path":dir.path().display().to_string(),"max_depth":"not-a-number"})
            .as_object()
            .unwrap()
            .clone(),
    );
    let response = bounded(sdk.service.call_tool(bad)).await;
    match response {
        Err(_) => {}
        Ok(result) => assert_eq!(result.is_error, Some(true)),
    }
    assert_result(
        bounded(sdk.scan(dir.path()).await?.await_response()).await?,
        dir.path(),
        1,
        7,
    );
    sdk.stop().await
}

#[tokio::test]
async fn sdk_cancel_notification_keeps_the_connection_usable() -> anyhow::Result<()> {
    let many = fixture(4096, 1);
    let next = fixture(1, 11);
    let mut sdk = Sdk::start().await?;
    let request = sdk.scan(many.path()).await?;
    let token: ProgressToken = request.progress_token.clone();
    let started = bounded(sdk.progress.recv())
        .await
        .expect("missing scan start");
    assert_eq!(started.progress_token, token);
    assert_eq!(started.progress, 0.0);
    bounded(request.cancel(Some("integration test cancellation".into()))).await?;
    // Do not rely on a timing race to assert that no work completed before cancellation.
    // Core tests prove stop semantics; this checks SDK/protocol cancellation and reuse.
    assert_result(
        bounded(sdk.scan(next.path()).await?.await_response()).await?,
        next.path(),
        1,
        11,
    );
    sdk.stop().await
}
