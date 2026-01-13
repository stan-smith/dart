//! Fallback image encoding and management
//!
//! Encodes a static image to H.264 at startup for use when sources disconnect.

use anyhow::{Context, Result};
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info};

/// Pre-encoded fallback frame data
#[derive(Clone)]
pub struct FallbackFrame {
    /// H.264 encoded keyframe data
    pub data: Arc<Vec<u8>>,
}

impl FallbackFrame {
    /// Encode an image file to H.264 fallback frame
    pub fn from_image<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid path"))?;

        info!("Encoding fallback image: {}", path.display());

        // Initialize GStreamer if not already done
        gstreamer::init().ok();

        // Pipeline to encode image to H.264 keyframe
        // filesrc -> decodebin -> videoconvert -> x264enc (single keyframe) -> h264parse -> appsink
        let pipeline_str = format!(
            "filesrc location=\"{path}\" \
             ! decodebin \
             ! videoconvert \
             ! videoscale \
             ! video/x-raw,width=640,height=480 \
             ! x264enc tune=stillimage key-int-max=1 \
             ! video/x-h264,stream-format=byte-stream,alignment=au \
             ! h264parse \
             ! appsink name=sink emit-signals=false sync=false",
            path = path_str
        );

        debug!("Fallback pipeline: {}", pipeline_str);

        let pipeline = gstreamer::parse::launch(&pipeline_str)
            .context("Failed to create fallback encoding pipeline")?
            .downcast::<gstreamer::Pipeline>()
            .map_err(|_| anyhow::anyhow!("Failed to downcast to Pipeline"))?;

        let sink = pipeline
            .by_name("sink")
            .ok_or_else(|| anyhow::anyhow!("Missing sink element"))?
            .dynamic_cast::<AppSink>()
            .map_err(|_| anyhow::anyhow!("Failed to cast to AppSink"))?;

        // Start pipeline
        pipeline
            .set_state(gstreamer::State::Playing)
            .map_err(|e| anyhow::anyhow!("Failed to start pipeline: {:?}", e))?;

        // Pull the encoded frame(s) - we just need one keyframe
        let mut frame_data = Vec::new();

        // Wait for up to 5 seconds for the frame
        let timeout = std::time::Duration::from_secs(5);
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            match sink.try_pull_sample(gstreamer::ClockTime::from_mseconds(100)) {
                Some(sample) => {
                    if let Some(buffer) = sample.buffer() {
                        if let Ok(map) = buffer.map_readable() {
                            frame_data.extend_from_slice(map.as_slice());
                            // For a still image, one frame is enough
                            break;
                        }
                    }
                }
                None => {
                    // Check if we hit EOS
                    if let Some(bus) = pipeline.bus() {
                        if bus.have_pending() {
                            for msg in bus.iter() {
                                if let gstreamer::MessageView::Eos(_) = msg.view() {
                                    break;
                                }
                                if let gstreamer::MessageView::Error(err) = msg.view() {
                                    pipeline.set_state(gstreamer::State::Null).ok();
                                    return Err(anyhow::anyhow!(
                                        "Fallback encoding error: {}",
                                        err.error()
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        // Stop pipeline
        pipeline.set_state(gstreamer::State::Null).ok();

        if frame_data.is_empty() {
            anyhow::bail!("Failed to encode fallback image - no data produced");
        }

        info!(
            "Fallback image encoded: {} bytes",
            frame_data.len()
        );

        Ok(Self {
            data: Arc::new(frame_data),
        })
    }

    /// Get the frame data
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}
