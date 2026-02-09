mod config;
mod config_wizard;
mod fallback;
mod rtsp;
mod sources;

use anyhow::Result;
use clap::Parser;
use config::{OutputCodec, SourceType};
use fallback::FallbackFrame;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "dart")]
#[command(about = "Universal RTSP restreamer - accepts V4L2 and RTSP inputs")]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    /// Interactively create a new configuration file
    #[arg(long)]
    config_new: bool,
}

fn main() -> Result<()> {
    // Parse CLI args
    let args = Args::parse();

    // Handle --config-new
    if args.config_new {
        return config_wizard::run(&args.config);
    }

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("dart=info".parse().unwrap()),
        )
        .init();

    // Initialize GStreamer
    gstreamer::init()?;
    info!("GStreamer initialized");

    // Detect MPP support once
    let mpp = sources::mpp_available();
    if mpp {
        info!("Rockchip MPP detected — using hardware H.265 encoding");
    } else {
        info!("MPP not available — using software x264 H.264 encoding");
    }

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

    // Track active source names for display and RTSP sources that need the Source abstraction
    let mut active_source_names: Vec<String> = Vec::new();
    let mut active_sources: Vec<Arc<sources::Source>> = Vec::new();

    for source_config in config.sources {
        info!(
            "Setting up source: {} ({:?})",
            source_config.name, source_config.source_type
        );

        match source_config.source_type {
            SourceType::V4l2 => {
                // V4L2 sources use direct factory launch — the RTSP server manages
                // the full pipeline. No appsrc, no Source thread needed.
                match rtsp_server.add_v4l2_mount(&source_config, mpp) {
                    Ok(()) => {
                        active_source_names.push(source_config.name.clone());
                    }
                    Err(e) => {
                        error!("Failed to add V4L2 mount for '{}': {}", source_config.name, e);
                    }
                }
            }
            SourceType::Rtsp => {
                // RTSP sources use appsrc pattern (rtspsrc has dynamic pads)
                let codec = if source_config.transcode && mpp {
                    OutputCodec::H265
                } else {
                    OutputCodec::H264
                };

                // Load fallback image if configured
                let fallback = if let Some(fallback_path) = &source_config.fallback {
                    match FallbackFrame::from_image(fallback_path, mpp) {
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

                let frame_tx = match rtsp_server.add_mount(&source_config, codec) {
                    Ok(tx) => tx,
                    Err(e) => {
                        error!("Failed to add mount for '{}': {}", source_config.name, e);
                        continue;
                    }
                };

                let source_name = source_config.name.clone();

                let source = match sources::Source::new(source_config, frame_tx, fallback, mpp) {
                    Ok(s) => Arc::new(s),
                    Err(e) => {
                        error!("Failed to create source '{}': {}", source_name, e);
                        rtsp_server.remove_mount(&source_name);
                        continue;
                    }
                };

                if let Err(e) = Arc::clone(&source).start() {
                    error!("Failed to start source '{}': {}", source_name, e);
                    rtsp_server.remove_mount(&source_name);
                    continue;
                }

                active_source_names.push(source_name);
                active_sources.push(source);
            }
        }
    }

    if active_source_names.is_empty() {
        anyhow::bail!("No sources started successfully");
    }

    info!("{} source(s) active", active_source_names.len());

    // Start RTSP server
    rtsp_server.start()?;

    // Print available streams
    println!("\nAvailable RTSP streams:");
    for name in &active_source_names {
        println!(
            "  rtsp://{}:{}/{}/stream",
            config.server.bind_address, config.server.rtsp_port, name
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
