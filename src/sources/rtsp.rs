//! RTSP source - receives streams from other RTSP servers
//!
//! Passthrough:       rtspsrc -> rtph264depay -> h264parse -> appsink
//! Transcode (x264):  rtspsrc -> rtph264depay -> avdec_h264 -> x264enc -> h264parse -> appsink
//! Transcode (MPP):   rtspsrc -> rtph264depay -> mppvideodec -> mpph265enc -> h265parse -> appsink

use crate::config::SourceConfig;
use anyhow::Result;
use gstreamer::prelude::*;
use tracing::debug;

use super::{appsink_config, build_encoder_string, build_mpp_h265_encoder_string, h264_caps, h265_caps};

/// Create RTSP source pipeline
pub fn create_pipeline(config: &SourceConfig, mpp: bool) -> Result<gstreamer::Pipeline> {
    let url = config
        .url
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("RTSP source requires 'url'"))?;

    let latency = config.latency.unwrap_or(200);

    // Build rtspsrc with optional auth
    let mut rtspsrc = format!("rtspsrc location=\"{}\" latency={}", url, latency);
    if let Some(user) = &config.username {
        rtspsrc.push_str(&format!(" user-id=\"{}\"", user));
    }
    if let Some(pass) = &config.password {
        rtspsrc.push_str(&format!(" user-pw=\"{}\"", pass));
    }

    let pipeline_str = if config.transcode {
        let encode = config.encode_config();

        if mpp {
            // MPP transcode: hardware decode + hardware H.265 encode
            let encoder = build_mpp_h265_encoder_string(&encode);

            format!(
                "{rtspsrc} \
                 ! rtph264depay \
                 ! mppvideodec \
                 ! {encoder} \
                 ! {h265_caps} \
                 ! h265parse \
                 ! {h265_caps} \
                 ! {appsink}",
                rtspsrc = rtspsrc,
                encoder = encoder,
                h265_caps = h265_caps(),
                appsink = appsink_config(),
            )
        } else {
            // x264 transcode (existing behavior)
            let encoder = build_encoder_string(&encode);

            format!(
                "{rtspsrc} \
                 ! rtph264depay \
                 ! avdec_h264 \
                 ! {encoder} \
                 ! {h264_caps} \
                 ! h264parse \
                 ! {h264_caps} \
                 ! {appsink}",
                rtspsrc = rtspsrc,
                encoder = encoder,
                h264_caps = h264_caps(),
                appsink = appsink_config(),
            )
        }
    } else {
        // Passthrough - always H.264, no changes needed
        format!(
            "{rtspsrc} \
             ! rtph264depay \
             ! h264parse \
             ! {h264_caps} \
             ! {appsink}",
            rtspsrc = rtspsrc,
            h264_caps = h264_caps(),
            appsink = appsink_config(),
        )
    };

    debug!("RTSP pipeline: {}", pipeline_str);

    let pipeline = gstreamer::parse::launch(&pipeline_str)?
        .downcast::<gstreamer::Pipeline>()
        .map_err(|_| anyhow::anyhow!("Failed to create pipeline"))?;

    Ok(pipeline)
}
