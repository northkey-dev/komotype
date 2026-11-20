//! Named pipe server: streams [`Status`] as JSON lines to every connected client.

use std::time::Duration;

use anyhow::{Context, Result};
use komotype_protocol::ipc::{PIPE_NAME, Status};
use tokio::io::AsyncWriteExt;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::watch;
use tracing::debug;

/// Clients get a line at least this often, so they can detect a hung daemon.
const KEEPALIVE: Duration = Duration::from_secs(5);

/// Creates the first pipe instance. Fails if another daemon already owns the pipe.
pub fn bind() -> Result<NamedPipeServer> {
    ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .create(PIPE_NAME)
        .context("another komotyped is already running (pipe in use)")
}

pub async fn serve(mut server: NamedPipeServer, status: watch::Receiver<Status>) -> Result<()> {
    loop {
        server.connect().await?;
        let client = server;
        server = ServerOptions::new()
            .reject_remote_clients(true)
            .create(PIPE_NAME)?;
        let rx = status.clone();
        tokio::spawn(async move {
            if let Err(e) = stream(client, rx).await {
                debug!("client disconnected: {e}");
            }
        });
    }
}

async fn stream(mut pipe: NamedPipeServer, mut rx: watch::Receiver<Status>) -> Result<()> {
    loop {
        let mut line = serde_json::to_vec(&*rx.borrow_and_update())?;
        line.push(b'\n');
        pipe.write_all(&line).await?;
        let _ = tokio::time::timeout(KEEPALIVE, rx.changed()).await;
    }
}
