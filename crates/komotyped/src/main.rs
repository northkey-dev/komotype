//! komotyped: background daemon of komotype, the gamified typing tracker for komorebi-bar.

mod config;
mod db;
mod devices;
mod engine;
mod firmware;
mod input;
mod ipc;
mod stats;

use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use komotype_protocol::ipc::{PIPE_NAME, Status};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::windows::named_pipe::ClientOptions;
use tracing::info;

use crate::devices::DeviceMatcher;
use crate::engine::{Engine, Event};
use crate::input::InputEvent;

#[derive(Parser)]
#[command(
    version,
    about = "komotype daemon: tracks typing on your main keyboard"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon (default).
    Run,
    /// List keyboards.
    Devices,
    /// Choose the main keyboard by pressing a key on it.
    Pick,
    /// Print the daemon's current status.
    Status {
        /// Keep printing updates.
        #[arg(long)]
        watch: bool,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "komotyped=info".into()),
        )
        .with_target(false)
        .init();

    match Cli::parse().command.unwrap_or(Command::Run) {
        Command::Run => run(),
        Command::Devices => {
            list_devices();
            Ok(())
        }
        Command::Pick => pick(),
        Command::Status { watch } => status(watch),
    }
}

fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let server = ipc::bind()?;
        let db = db::Db::open(&config::data_dir().join("komotype.db"))?;
        let (status_tx, status_rx) = tokio::sync::watch::channel(Status::default());
        let engine = Engine::new(db, status_tx)?;

        let (tx, rx) = mpsc::channel();
        input::spawn(tx.clone())?;
        firmware::spawn(tx);
        std::thread::Builder::new()
            .name("engine".into())
            .spawn(move || engine.run(rx))?;

        info!(config = %config::path().display(), "komotyped running");
        ipc::serve(server, status_rx).await
    })
}

fn list_devices() {
    let config = config::load().unwrap_or_default();
    let devices = devices::list();
    if devices.is_empty() {
        println!("No keyboards found.");
    }
    for (_, info) in devices {
        let main = config.main.as_ref().is_some_and(|m| m.matches(&info));
        let ids = match (info.vid, info.pid) {
            (Some(v), Some(p)) => format!("{v:04X}:{p:04X}"),
            _ => "????:????".into(),
        };
        println!(
            "{} {:<28} {ids}  {:?}",
            if main { "*" } else { " " },
            info.display_name(),
            info.transport
        );
    }
    println!("\n* = main keyboard. Choose with `komotyped pick`.");
}

fn pick() -> Result<()> {
    let (tx, rx) = mpsc::channel();
    input::spawn(tx)?;
    println!("Press any key on the keyboard you want to track...");
    let handle = loop {
        if let Event::Input(InputEvent::Key {
            device,
            pressed: true,
            ..
        }) = rx.recv()?
        {
            break device;
        }
    };
    let info = devices::resolve(handle).context("could not identify that keyboard")?;
    let matcher = DeviceMatcher::from_info(&info).context("this keyboard reports no VID/PID")?;
    let mut config = config::load()?;
    config.main = Some(matcher);
    config::save(&config)?;
    println!(
        "Main keyboard: {} ({:?})",
        info.display_name(),
        info.transport
    );
    println!("Saved to {}", config::path().display());
    Ok(())
}

fn status(watch: bool) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let pipe = ClientOptions::new()
            .open(PIPE_NAME)
            .context("komotyped is not running")?;
        let mut lines = BufReader::new(pipe).lines();
        while let Some(line) =
            tokio::time::timeout(Duration::from_secs(10), lines.next_line()).await??
        {
            let s: Status = serde_json::from_str(&line)?;
            if !watch {
                println!("{}", serde_json::to_string_pretty(&s)?);
                return Ok(());
            }
            print_line(&s);
        }
        Ok(())
    })
}

fn print_line(s: &Status) {
    let t = &s.typing;
    let pct = |v: Option<f32>| v.map_or("-".into(), |v| format!("{:.0}%", v * 100.0));
    let fw = s.firmware.as_ref().map_or(String::new(), |f| {
        let bat = |b: Option<u8>| b.map_or("?".into(), |b| format!("{b}%"));
        let right = f.report.battery_peripherals.first().copied().flatten();
        format!(
            " | L{} | bat {} / {} ({:?})",
            f.report.layer,
            bat(f.report.battery_central),
            bat(right),
            f.transport
        )
    });
    println!(
        "{:>5.1} WPM | today {} words, acc {}, best {} | share {}{fw}",
        t.wpm_live,
        t.words_today,
        pct(t.accuracy_today),
        t.best_wpm_today.map_or("-".into(), |w| format!("{w:.0}")),
        pct(s.main_share_today),
    );
}
