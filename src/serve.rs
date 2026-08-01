//! `h serve`: a headless, multi-session JSON-RPC API over stdio.
//!
//! The terminal UI is untouched; this is the face IDE integrations talk to.
//! Lifecycle: `server/hello` on startup, requests and event notifications
//! until stdin closes, `server/shutdown` arrives, or a termination signal,
//! then every session archives and the process exits cleanly.

pub mod protocol;
mod session;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

use crate::cli::ServeArgs;

use session::{AgentSessionBuilder, SessionManager};

/// Serves the JSON-RPC protocol until shutdown, then archives everything.
pub async fn run(args: ServeArgs) -> anyhow::Result<()> {
    tracing::info!(event = "serve.starting", profile = ?args.profile);

    let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // A single writer task owns stdout, so protocol frames from concurrent
    // sessions never interleave mid-line.
    tokio::spawn(async move {
        let stdout = tokio::io::stdout();
        let mut writer = BufWriter::new(stdout);

        while let Some(line) = line_rx.recv().await {
            if let Err(error) = write_frame(&mut writer, &line).await {
                tracing::warn!(
                    event = "serve.output.failed",
                    error_class = "stdout_write",
                    error = error.to_string(),
                );
                break;
            }
        }
    });

    line_tx.send(protocol::hello(env!("CARGO_PKG_VERSION"), std::process::id())).ok();

    let mut manager = SessionManager::new(
        line_tx.clone(),
        Box::new(AgentSessionBuilder),
        args.profile,
    );

    let mut lines = BufReader::new(tokio::io::stdin()).lines();

    loop {
        tokio::select! {
            line = lines.next_line() => match line? {
                Some(line) => {
                    if manager.handle(&line).await? {
                        break;
                    }
                }
                // The client went away: archive everything and exit.
                None => break,
            },
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate_signal() => break,
        }
    }

    tracing::info!(event = "serve.shutting_down");
    manager.shutdown().await;
    tracing::info!(event = "serve.stopped");
    Ok(())
}

async fn write_frame(writer: &mut BufWriter<tokio::io::Stdout>, line: &str) -> std::io::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

/// SIGTERM, matching how process managers and VS Code tear the server down.
#[cfg(unix)]
async fn terminate_signal() {
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");
    signal.recv().await;
}

#[cfg(not(unix))]
async fn terminate_signal() {
    std::future::pending::<()>().await
}
