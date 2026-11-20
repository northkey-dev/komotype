//! Named pipe server: streams [`Status`] as JSON lines to every connected client and accepts
//! [`Command`] lines from them.

use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::{Context, Result};
use komotype_protocol::ipc::{Command, PIPE_NAME, Status};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::watch;
use tracing::{debug, warn};

use crate::engine::Event;

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

pub async fn serve(
    mut server: NamedPipeServer,
    status: watch::Receiver<Status>,
    events: Sender<Event>,
) -> Result<()> {
    loop {
        server.connect().await?;
        let client = server;
        server = ServerOptions::new()
            .reject_remote_clients(true)
            .create(PIPE_NAME)?;
        let rx = status.clone();
        let events = events.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(client, rx, events).await {
                debug!("client disconnected: {e}");
            }
        });
    }
}

async fn handle(
    pipe: NamedPipeServer,
    mut rx: watch::Receiver<Status>,
    events: Sender<Event>,
) -> Result<()> {
    let (reader, mut writer) = tokio::io::split(pipe);
    let commands = tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            match serde_json::from_str::<Command>(&line) {
                Ok(command) => {
                    if events.send(Event::Command(command)).is_err() {
                        break;
                    }
                }
                Err(e) => warn!("ignoring unknown command {line:?}: {e}"),
            }
        }
    });
    let result = async {
        loop {
            let mut line = serde_json::to_vec(&*rx.borrow_and_update())?;
            line.push(b'\n');
            writer.write_all(&line).await?;
            let _ = tokio::time::timeout(KEEPALIVE, rx.changed()).await;
        }
    }
    .await;
    commands.abort();
    result
}
