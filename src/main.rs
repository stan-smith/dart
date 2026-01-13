mod config;
mod fallback;
mod rtsp;
mod sources;

use anyhow::Result;
use clap::Parser;
use fallback::FallbackFrame;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "simplertsp")]
#[command(about = "Universal RTSP restreamer - accepts V4L2 and RTSP inputs")]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,
}

fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("dart=info".parse().unwrap()),
        )
        .init();

    // Parse CLI args
    let args = Args::parse();

    // Initialize GStreamer
    gstreamer::init()?;
    info!("GStreamer initialized");

    // Load configuration
    let config = config::Config::load(&args.config)?;
    info!("Loaded config from: {}", args.config.display());
    info!(
        "Server: {}:{}, {} source(s)",
        config.server.bind_address,
        config.server.rtsp_port,
        config.sources.len()
    );

    // Create RTSP server
    let rtsp_server = rtsp::RtspServer::new(config.server.rtsp_port, &config.server.bind_address)?;

    // Create and start sources
    let mut active_sources: Vec<Arc<sources::Source>> = Vec::new();

    for source_config in config.sources {
        info!(
            "Setting up source: {} ({:?})",
            source_config.name, source_config.source_type
        );

        // Load fallback image if configured
        let fallback = if let Some(fallback_path) = &source_config.fallback {
            match FallbackFrame::from_image(fallback_path) {
                Ok(f) => {
                    info!(
                        "Loaded fallback image for '{}': {}",
                        source_config.name, fallback_path
                    );
                    Some(f)
                }
                Err(e) => {
                    warn!(
                        "Failed to load fallback image for '{}': {}",
                        source_config.name, e
                    );
                    None
                }
            }
        } else {
            None
        };

        // Add mount point and get frame sender
        let frame_tx = match rtsp_server.add_mount(&source_config) {
            Ok(tx) => tx,
            Err(e) => {
                error!("Failed to add mount for '{}': {}", source_config.name, e);
                continue;
            }
        };

        let source_name = source_config.name.clone();

        // Create source
        let source = match sources::Source::new(source_config, frame_tx, fallback) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                error!("Failed to create source '{}': {}", source_name, e);
                rtsp_server.remove_mount(&source_name);
                continue;
            }
        };

        // Start source
        if let Err(e) = Arc::clone(&source).start() {
            error!("Failed to start source '{}': {}", source_name, e);
            rtsp_server.remove_mount(&source_name);
            continue;
        }

        active_sources.push(source);
    }

    if active_sources.is_empty() {
        anyhow::bail!("No sources started successfully");
    }

    info!("{} source(s) active", active_sources.len());

    // Start RTSP server
    rtsp_server.start()?;

    // Print available streams
    println!("\nAvailable RTSP streams:");
    for source in &active_sources {
        println!(
            "  rtsp://{}:{}/{}/stream",
            config.server.bind_address, config.server.rtsp_port, source.name()
        );
    }
    println!();

    // Wait for Ctrl+C
    info!("Press Ctrl+C to stop");
    let (tx, rx) = std::sync::mpsc::channel();
    ctrlc::set_handler(move || {
        let _ = tx.send(());
    })
    .expect("Error setting Ctrl+C handler");

    rx.recv().ok();

    // Shutdown
    info!("Shutting down...");
    for source in &active_sources {
        source.stop();
    }
    rtsp_server.stop();

    info!("Goodbye!");
    Ok(())
}
