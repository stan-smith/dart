//! V4L2 source - captures from Video4Linux2 devices (webcams, capture cards)
//!
//! Pipeline: v4l2src -> videoconvert -> x264enc -> h264parse -> appsink

use crate::config::SourceConfig;
use anyhow::Result;
use gstreamer::prelude::*;
use tracing::debug;

use super::{appsink_config, build_encoder_string, h264_caps};

/// Create V4L2 capture pipeline
pub fn create_pipeline(config: &SourceConfig) -> Result<gstreamer::Pipeline> {
    let device = config
        .device
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("V4L2 source requires 'device'"))?;

    let encode = config.encode_config();

    let encoder = build_encoder_string(&encode);

    // Build source caps - if format is specified (for capture cards like TC358743),
    // use it with bt601 colorimetry. Otherwise let device negotiate freely.
    let source_caps = if let Some(format) = &config.format {
        // For capture cards that need explicit format and colorimetry
        let mut caps_parts = vec![format!("format={}", format)];
        if let Some(w) = config.width {
            caps_parts.push(format!("width={}", w));
        }
        if let Some(h) = config.height {
            caps_parts.push(format!("height={}", h));
        }
        // bt601 colorimetry is required for most HDMI capture cards
        caps_parts.push("colorimetry=bt601".to_string());
        format!(" ! video/x-raw,{}", caps_parts.join(","))
    } else {
        // Let v4l2src negotiate freely (for regular webcams)
        String::new()
    };

    // Build output caps for after conversion
    let output_caps = match (config.width, config.height, config.framerate) {
        (Some(w), Some(h), Some(f)) => {
            format!("video/x-raw,width={},height={},framerate={}/1", w, h, f)
        }
        (Some(w), Some(h), None) => format!("video/x-raw,width={},height={}", w, h),
        _ => String::from("video/x-raw"),
    };

    let pipeline_str = format!(
        "v4l2src device={device}{source_caps} \
         ! videoconvert \
         ! videoscale \
         ! {output_caps} \
         ! {encoder} \
         ! {h264_caps} \
         ! h264parse \
         ! {h264_caps} \
         ! {appsink}",
        device = device,
        source_caps = source_caps,
        output_caps = output_caps,
        encoder = encoder,
        h264_caps = h264_caps(),
        appsink = appsink_config(),
    );

    debug!("V4L2 pipeline: {}", pipeline_str);

    let pipeline = gstreamer::parse::launch(&pipeline_str)?
        .downcast::<gstreamer::Pipeline>()
        .map_err(|_| anyhow::anyhow!("Failed to create pipeline"))?;

    Ok(pipeline)
}
