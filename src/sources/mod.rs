pub mod rtsp;
pub mod v4l2;

use crate::config::{EncodeConfig, SourceConfig, SourceType};
use crate::fallback::FallbackFrame;
use crate::rtsp::{FrameData, FrameSender};
use anyhow::Result;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Check if Rockchip MPP H.265 encoder is available
pub fn mpp_available() -> bool {
    gstreamer::ElementFactory::find("mpph265enc").is_some()
}

/// Source state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    /// Streaming live from source
    Live,
    /// Source disconnected, showing fallback
    Fallback,
    /// Stopped
    Stopped,
}

/// Common source functionality with fallback support
pub struct Source {
    name: String,
    config: SourceConfig,
    frame_tx: Arc<Mutex<Option<FrameSender>>>,
    fallback: Option<FallbackFrame>,
    state: Arc<Mutex<SourceState>>,
    running: Arc<AtomicBool>,
    mpp: bool,
}

impl Source {
    /// Create a new source from configuration
    pub fn new(
        config: SourceConfig,
        frame_tx: Arc<Mutex<Option<FrameSender>>>,
        fallback: Option<FallbackFrame>,
        mpp: bool,
    ) -> Result<Self> {
        Ok(Self {
            name: config.name.clone(),
            config,
            frame_tx,
            fallback,
            state: Arc::new(Mutex::new(SourceState::Stopped)),
            running: Arc::new(AtomicBool::new(false)),
            mpp,
        })
    }

    /// Start the source with automatic reconnection
    pub fn start(self: Arc<Self>) -> Result<()> {
        self.running.store(true, Ordering::SeqCst);
        *self.state.lock().unwrap() = SourceState::Live;

        let source = Arc::clone(&self);
        std::thread::spawn(move || {
            source.run_loop();
        });

        info!("Started source: {}", self.name);
        Ok(())
    }

    /// Main run loop with reconnection logic
    fn run_loop(&self) {
        // Fast poll interval for recovery (2 seconds)
        const FAST_POLL_INTERVAL: Duration = Duration::from_secs(2);

        while self.running.load(Ordering::SeqCst) {
            // Try to create and run the pipeline
            match self.create_and_run_pipeline() {
                Ok(()) => {
                    // Pipeline ended normally (EOS) - try to reconnect
                    if !self.running.load(Ordering::SeqCst) {
                        break;
                    }
                    info!("Source '{}' ended, will reconnect", self.name);
                }
                Err(e) => {
                    error!("Source '{}' error: {}", self.name, e);
                }
            }

            // Switch to fallback mode (only for RTSP sources)
            // V4L2 devices just log error and retry
            if self.config.source_type == SourceType::Rtsp && self.fallback.is_some() {
                *self.state.lock().unwrap() = SourceState::Fallback;
                info!("Source '{}' switched to fallback mode", self.name);

                // Start fallback frame sender
                self.start_fallback_sender();
            } else if self.config.source_type == SourceType::V4l2 {
                warn!("Source '{}': V4L2 device not available, retrying...", self.name);
            }

            // Fast polling loop - try to reconnect quickly
            loop {
                if !self.running.load(Ordering::SeqCst) {
                    break;
                }

                debug!(
                    "Source '{}' checking connectivity in {:?}...",
                    self.name, FAST_POLL_INTERVAL
                );
                std::thread::sleep(FAST_POLL_INTERVAL);

                // Quick probe to check if source is available
                if self.probe_source() {
                    info!("Source '{}' appears to be available, reconnecting...", self.name);
                    break;
                }
            }
        }

        *self.state.lock().unwrap() = SourceState::Stopped;
        debug!("Source '{}' run loop ended", self.name);
    }

    /// Quick probe to check if source is available without starting full pipeline
    fn probe_source(&self) -> bool {
        match self.config.source_type {
            SourceType::Rtsp => self.probe_rtsp(),
            SourceType::V4l2 => self.probe_v4l2(),
        }
    }

    /// Probe RTSP source by attempting a quick connection
    fn probe_rtsp(&self) -> bool {
        let url = match &self.config.url {
            Some(u) => u,
            None => return false,
        };

        // Try to create a minimal pipeline just to test connectivity
        // Use a short timeout (2 seconds)
        let mut pipeline_str = format!(
            "rtspsrc location=\"{}\" latency=0 timeout=2000000 ! fakesink",
            url
        );

        if let Some(user) = &self.config.username {
            pipeline_str = format!(
                "rtspsrc location=\"{}\" latency=0 timeout=2000000 user-id=\"{}\"",
                url, user
            );
            if let Some(pass) = &self.config.password {
                pipeline_str.push_str(&format!(" user-pw=\"{}\"", pass));
            }
            pipeline_str.push_str(" ! fakesink");
        }

        let pipeline = match gstreamer::parse::launch(&pipeline_str) {
            Ok(p) => p,
            Err(_) => return false,
        };

        // Try to set to PAUSED (will attempt connection)
        let result = pipeline.set_state(gstreamer::State::Paused);

        // Wait briefly for state change
        if result.is_ok() {
            let bus = pipeline.bus();
            if let Some(bus) = bus {
                // Wait up to 2 seconds for state change or error
                for _ in 0..20 {
                    if let Some(msg) = bus.timed_pop(gstreamer::ClockTime::from_mseconds(100)) {
                        match msg.view() {
                            gstreamer::MessageView::Error(_) => {
                                pipeline.set_state(gstreamer::State::Null).ok();
                                return false;
                            }
                            gstreamer::MessageView::StateChanged(state) => {
                                if state.current() == gstreamer::State::Paused {
                                    pipeline.set_state(gstreamer::State::Null).ok();
                                    return true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        pipeline.set_state(gstreamer::State::Null).ok();
        false
    }

    /// Probe V4L2 device by trying to negotiate caps
    fn probe_v4l2(&self) -> bool {
        let device = match &self.config.device {
            Some(d) => d,
            None => return false,
        };

        // First check if device exists
        if !std::path::Path::new(device).exists() {
            return false;
        }

        // Try to create a minimal pipeline to test if we can negotiate caps
        // This will fail if there's no signal (for capture cards like TC358743)
        let caps = if let Some(format) = &self.config.format {
            let mut parts = vec![format!("format={}", format)];
            if let Some(w) = self.config.width {
                parts.push(format!("width={}", w));
            }
            if let Some(h) = self.config.height {
                parts.push(format!("height={}", h));
            }
            parts.push("colorimetry=bt601".to_string());
            format!(" ! video/x-raw,{}", parts.join(","))
        } else {
            String::new()
        };

        let pipeline_str = format!(
            "v4l2src device={}{} ! fakesink",
            device, caps
        );

        let pipeline = match gstreamer::parse::launch(&pipeline_str) {
            Ok(p) => p,
            Err(_) => return false,
        };

        // Try to set to PAUSED - this will attempt to negotiate caps
        let result = pipeline.set_state(gstreamer::State::Paused);
        if result.is_err() {
            pipeline.set_state(gstreamer::State::Null).ok();
            return false;
        }

        // Wait for state change or error
        if let Some(bus) = pipeline.bus() {
            for _ in 0..20 {
                if let Some(msg) = bus.timed_pop(gstreamer::ClockTime::from_mseconds(100)) {
                    match msg.view() {
                        gstreamer::MessageView::Error(_) => {
                            pipeline.set_state(gstreamer::State::Null).ok();
                            return false;
                        }
                        gstreamer::MessageView::StateChanged(state) => {
                            if state.current() == gstreamer::State::Paused {
                                pipeline.set_state(gstreamer::State::Null).ok();
                                return true;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        pipeline.set_state(gstreamer::State::Null).ok();
        false
    }

    /// Create and run the pipeline, returns when pipeline ends or errors
    fn create_and_run_pipeline(&self) -> Result<()> {
        let pipeline = match self.config.source_type {
            SourceType::V4l2 => v4l2::create_pipeline(&self.config, self.mpp)?,
            SourceType::Rtsp => rtsp::create_pipeline(&self.config, self.mpp)?,
        };

        // Set up appsink callbacks
        let frame_tx = Arc::clone(&self.frame_tx);
        let name = self.name.clone();
        let state = Arc::clone(&self.state);

        setup_appsink_callbacks(&pipeline, &name, frame_tx, state)?;

        // Start pipeline
        pipeline
            .set_state(gstreamer::State::Playing)
            .map_err(|e| anyhow::anyhow!("Failed to start pipeline: {:?}", e))?;

        *self.state.lock().unwrap() = SourceState::Live;
        info!("Source '{}' pipeline started", self.name);

        // Wait for pipeline to end or error
        let bus = pipeline
            .bus()
            .ok_or_else(|| anyhow::anyhow!("No bus on pipeline"))?;

        loop {
            if !self.running.load(Ordering::SeqCst) {
                break;
            }

            // Poll bus with timeout
            if let Some(msg) = bus.timed_pop(gstreamer::ClockTime::from_mseconds(500)) {
                match msg.view() {
                    gstreamer::MessageView::Error(err) => {
                        pipeline.set_state(gstreamer::State::Null).ok();
                        return Err(anyhow::anyhow!(
                            "Pipeline error: {} ({:?})",
                            err.error(),
                            err.debug()
                        ));
                    }
                    gstreamer::MessageView::Eos(_) => {
                        debug!("Source '{}' reached EOS", self.name);
                        break;
                    }
                    gstreamer::MessageView::Warning(warn) => {
                        warn!(
                            "Source '{}' warning: {} ({:?})",
                            self.name,
                            warn.error(),
                            warn.debug()
                        );
                    }
                    _ => {}
                }
            }
        }

        pipeline.set_state(gstreamer::State::Null).ok();
        Ok(())
    }

    /// Send fallback frames while in fallback state
    fn start_fallback_sender(&self) {
        let fallback = match &self.fallback {
            Some(f) => f.clone(),
            None => return,
        };

        let frame_tx = Arc::clone(&self.frame_tx);
        let state = Arc::clone(&self.state);
        let running = Arc::clone(&self.running);
        let name = self.name.clone();

        // Send fallback frames at ~1 fps while in fallback state
        std::thread::spawn(move || {
            debug!("Fallback sender started for '{}'", name);
            let frame_interval = Duration::from_secs(1);

            while running.load(Ordering::SeqCst) {
                // Check if we're still in fallback state
                if *state.lock().unwrap() != SourceState::Fallback {
                    break;
                }

                // Send fallback frame
                let frame = FrameData {
                    data: fallback.data().to_vec(),
                    is_keyframe: true,
                };

                if let Ok(guard) = frame_tx.lock() {
                    if let Some(tx) = guard.as_ref() {
                        if tx.send(frame).is_err() {
                            debug!("Fallback sender '{}': receiver disconnected", name);
                        }
                    }
                }

                std::thread::sleep(frame_interval);
            }

            debug!("Fallback sender ended for '{}'", name);
        });
    }

    /// Stop the source
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        *self.state.lock().unwrap() = SourceState::Stopped;
        info!("Stopped source: {}", self.name);
    }

    /// Get source name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get current state
    pub fn state(&self) -> SourceState {
        *self.state.lock().unwrap()
    }
}

/// Set up appsink callbacks to receive frames
fn setup_appsink_callbacks(
    pipeline: &gstreamer::Pipeline,
    name: &str,
    frame_tx: Arc<Mutex<Option<FrameSender>>>,
    state: Arc<Mutex<SourceState>>,
) -> Result<()> {
    let sink = pipeline
        .by_name("sink")
        .ok_or_else(|| anyhow::anyhow!("Pipeline missing 'sink' element"))?;

    let appsink = sink
        .dynamic_cast::<AppSink>()
        .map_err(|_| anyhow::anyhow!("Failed to cast to AppSink"))?;

    let name = name.to_string();

    appsink.set_callbacks(
        gstreamer_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                // Only send frames when in Live state
                if *state.lock().unwrap() != SourceState::Live {
                    return Ok(gstreamer::FlowSuccess::Ok);
                }

                let sample = sink.pull_sample().map_err(|_| gstreamer::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gstreamer::FlowError::Error)?;
                let map = buffer.map_readable().map_err(|_| gstreamer::FlowError::Error)?;

                // Check if this is a keyframe (no DELTA_UNIT flag)
                let is_keyframe = !buffer.flags().contains(gstreamer::BufferFlags::DELTA_UNIT);

                let frame = FrameData {
                    data: map.as_slice().to_vec(),
                    is_keyframe,
                };

                // Send frame if we have a receiver
                if let Ok(guard) = frame_tx.lock() {
                    if let Some(tx) = guard.as_ref() {
                        if tx.send(frame).is_err() {
                            debug!("Source '{}': frame receiver disconnected", name);
                        }
                    }
                }

                Ok(gstreamer::FlowSuccess::Ok)
            })
            .build(),
    );

    Ok(())
}

/// Build encoder pipeline string
pub fn build_encoder_string(encode: &EncodeConfig) -> String {
    format!(
        "videoconvert ! x264enc bitrate={} key-int-max={} speed-preset={} tune={}",
        encode.bitrate, // bitrate is in kbps
        encode.keyframe_interval,
        encode.preset,
        encode.tune
    )
}

/// Common appsink configuration
pub fn appsink_config() -> &'static str {
    "appsink name=sink emit-signals=true sync=false"
}

/// H.264 output caps
pub fn h264_caps() -> &'static str {
    "video/x-h264,stream-format=byte-stream,alignment=au"
}

/// H.265 output caps
pub fn h265_caps() -> &'static str {
    "video/x-h265,stream-format=byte-stream,alignment=au"
}

/// Build MPP H.265 encoder pipeline string
pub fn build_mpp_h265_encoder_string(encode: &EncodeConfig) -> String {
    format!(
        "mpph265enc bps={} gop={}",
        encode.bitrate * 1000, // config is kbps, MPP wants bps
        encode.keyframe_interval,
    )
}
