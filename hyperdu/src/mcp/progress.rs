//! Bounded progress delivery. The scanner never waits on the protocol transport.
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use hyperdu_core::{Options, StatMap};
use rmcp::{model::ProgressNotificationParam, service::RequestContext, ErrorData, RoleServer};

pub(super) async fn cancelled(context: &Option<RequestContext<RoleServer>>) {
    if let Some(context) = context {
        context.ct.cancelled().await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn notify(
    context: &RequestContext<RoleServer>,
    notification: ProgressNotificationParam,
) -> Result<(), ErrorData> {
    tokio::select! {
        _ = context.ct.cancelled() => Err(ErrorData::internal_error("scan cancelled", None)),
        result = context.peer.notify_progress(notification) => result.map_err(|e|
            ErrorData::internal_error(format!("progress transport failed: {e}"), None)),
    }
}

pub(super) async fn scan(
    root: PathBuf,
    max_depth: u32,
    context: Option<RequestContext<RoleServer>>,
) -> Result<(StatMap, u64), ErrorData> {
    let token = context.as_ref().and_then(|c| c.meta.get_progress_token());
    let (updates, mut latest) = tokio::sync::watch::channel(0u64);
    let mut options = Options {
        max_depth,
        progress_every: 0,
        ..Options::default()
    };
    if let (Some(context), Some(token)) = (&context, &token) {
        notify(
            context,
            ProgressNotificationParam::new(token.clone(), 0.0)
                .with_message(format!("Scanning {}", root.display())),
        )
        .await?;
        options.progress_every = 256;
        options.progress_callback = Some(Arc::new(move |files| {
            updates.send_if_modified(|latest| {
                if files > *latest {
                    *latest = files;
                    true
                } else {
                    false
                }
            });
        }));
    }
    let scan_root = root.clone();
    let started = Instant::now();
    let mut worker = super::blocking::start(
        super::blocking::capacity(),
        cancelled(&context),
        move |cancel| {
            options.cancel = cancel;
            hyperdu_core::scan_directory(scan_root, &options)
        },
    )
    .await?;
    let mut pending = false;
    let mut open = token.is_some();
    let mut sent = 0u64;
    let mut next = tokio::time::Instant::now();
    let stats = loop {
        tokio::select! {
            biased;
            _ = cancelled(&context) => {
                worker.cancel_and_wait().await;
                return Err(ErrorData::internal_error("scan cancelled", None));
            },
            result = &mut worker.handle => break result
                .map_err(|e| ErrorData::internal_error(format!("scan task failed: {e}"), None))?
                .map_err(|e| ErrorData::internal_error(format!("scan of {} failed: {e}", root.display()), None))?,
            _ = tokio::time::sleep_until(next), if pending => {
                let files = *latest.borrow_and_update();
                if files > sent {
                    if let (Some(context), Some(token)) = (&context, &token) {
                        if let Err(error) = notify(context, ProgressNotificationParam::new(token.clone(), files as f64)
                            .with_message(format!("Scanned {files} files in {:.1}s", started.elapsed().as_secs_f64()))).await {
                            worker.cancel_and_wait().await;
                            return Err(error);
                        }
                    }
                    sent = files;
                }
                pending = false;
                next = tokio::time::Instant::now() + Duration::from_millis(250);
            }
            changed = latest.changed(), if open => {
                if changed.is_err() { open = false; }
                else { pending = true; }
            }
        }
    };
    let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let files = stats.get(&root).map(|s| s.files).unwrap_or(0);
    if files > sent {
        if let (Some(context), Some(token)) = (&context, &token) {
            notify(
                context,
                ProgressNotificationParam::new(token.clone(), files as f64).with_message(format!(
                    "Scan complete: {files} files, {} directories, {elapsed} ms",
                    stats.len()
                )),
            )
            .await?;
        }
    }
    Ok((stats, elapsed))
}
