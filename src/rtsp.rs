use crate::config::{AuthConfig, SourceConfig};
use anyhow::Result;
use gstreamer::prelude::*;
use gstreamer_app::AppSrc;
use gstreamer_rtsp_server::prelude::*;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info, warn};

/// Frame data sent from source to RTSP output
pub struct FrameData {
    pub data: Vec<u8>,
    pub is_keyframe: bool,
}

/// Handle to send frames to an RTSP output
pub type FrameSender = Sender<FrameData>;

/// RTSP server wrapper
pub struct RtspServer {
    server: gstreamer_rtsp_server::RTSPServer,
    mounts: gstreamer_rtsp_server::RTSPMountPoints,
    main_loop: glib::MainLoop,
    port: u16,
}

impl RtspServer {
    /// Create a new RTSP server
    pub fn new(port: u16, bind_address: &str) -> Result<Self> {
        let server = gstreamer_rtsp_server::RTSPServer::new();
        server.set_service(&port.to_string());
        server.set_address(bind_address);

        let mounts = server
            .mount_points()
            .ok_or_else(|| anyhow::anyhow!("Failed to get mount points"))?;

        let main_loop = glib::MainLoop::new(None, false);

        Ok(Self {
            server,
            mounts,
            main_loop,
            port,
        })
    }

    /// Start the RTSP server in a background thread
    pub fn start(&self) -> Result<()> {
        let main_loop = self.main_loop.clone();

        // Attach server to default main context
        let _source_id = self.server.attach(None);

        // Run main loop in separate thread
        std::thread::spawn(move || {
            main_loop.run();
        });

        info!(
            "RTSP server started on {}:{}",
            self.server.address().unwrap_or_else(|| "0.0.0.0".into()),
            self.port
        );

        Ok(())
    }

    /// Add a stream mount point for a source
    /// Returns a channel sender that can be used to push frames
    pub fn add_mount(
        &self,
        source: &SourceConfig,
    ) -> Result<Arc<Mutex<Option<FrameSender>>>> {
        let mount_path = format!("/{}/stream", source.name);

        // Create factory with appsrc pipeline
        let factory = gstreamer_rtsp_server::RTSPMediaFactory::new();
        factory.set_launch(
            "( appsrc name=videosrc is-live=true format=time do-timestamp=true \
               caps=video/x-h264,stream-format=byte-stream,alignment=au \
               ! h264parse \
               ! rtph264pay name=pay0 pt=96 )",
        );
        factory.set_shared(true);

        // Set up authentication if configured
        if let Some(auth_config) = &source.auth {
            if auth_config.enabled {
                if let Err(e) = self.setup_auth(auth_config) {
                    warn!("Failed to setup auth for '{}': {}", source.name, e);
                }
            }
        }

        // Channel for frames - initially None, populated when client connects
        let frame_tx: Arc<Mutex<Option<FrameSender>>> = Arc::new(Mutex::new(None));
        let frame_tx_clone = Arc::clone(&frame_tx);
        let source_name = source.name.clone();

        // Connect to media-configure signal
        factory.connect_media_configure(move |_factory, media| {
            let element = media.element();
            let Some(bin) = element.downcast_ref::<gstreamer::Bin>() else {
                error!("Failed to downcast media element to Bin");
                return;
            };

            let Some(appsrc_elem) = bin.by_name("videosrc") else {
                error!("Failed to find videosrc element in pipeline");
                return;
            };

            let Ok(appsrc) = appsrc_elem.dynamic_cast::<AppSrc>() else {
                error!("Failed to cast element to AppSrc");
                return;
            };

            // Create channel for this media instance
            let (tx, rx) = std::sync::mpsc::channel::<FrameData>();
            *frame_tx_clone.lock().unwrap() = Some(tx);

            let name = source_name.clone();

            // Spawn thread to push frames to appsrc
            std::thread::spawn(move || {
                let mut waiting_for_keyframe = true;
                let mut frame_count = 0u64;

                debug!("Frame pusher thread started for source '{}'", name);

                while let Ok(frame) = rx.recv() {
                    // Wait for keyframe before starting (cleaner playback start)
                    if waiting_for_keyframe {
                        if !frame.is_keyframe {
                            continue;
                        }
                        info!("Got initial keyframe for source '{}', starting stream", name);
                        waiting_for_keyframe = false;
                    }

                    // Create GStreamer buffer from frame data
                    let mut buffer = gstreamer::Buffer::from_slice(frame.data);
                    {
                        let buffer_ref = buffer.get_mut().unwrap();
                        if !frame.is_keyframe {
                            buffer_ref.set_flags(gstreamer::BufferFlags::DELTA_UNIT);
                        }
                    }

                    // Push buffer to appsrc
                    match appsrc.push_buffer(buffer) {
                        Ok(_) => {
                            frame_count += 1;
                            if frame_count % 300 == 0 {
                                debug!(
                                    "Source '{}': pushed {} frames",
                                    name, frame_count
                                );
                            }
                        }
                        Err(e) => {
                            debug!(
                                "Source '{}': appsrc push failed (pipeline closed?): {:?}",
                                name, e
                            );
                            break;
                        }
                    }
                }

                debug!(
                    "Frame pusher thread ended for source '{}' after {} frames",
                    name, frame_count
                );
            });
        });

        // Add factory to mount points
        self.mounts.add_factory(&mount_path, factory);
        info!("Added RTSP mount: rtsp://localhost:{}{}",
              self.port,
              mount_path);

        Ok(frame_tx)
    }

    /// Remove a mount point
    pub fn remove_mount(&self, name: &str) {
        let mount_path = format!("/{}/stream", name);
        self.mounts.remove_factory(&mount_path);
        info!("Removed RTSP mount: {}", mount_path);
    }

    /// Set up authentication on the server
    fn setup_auth(&self, auth_config: &AuthConfig) -> Result<()> {
        let username = auth_config
            .username
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Auth enabled but username not set"))?;
        let password = auth_config
            .password
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Auth enabled but password not set"))?;

        // Create auth handler
        let auth = gstreamer_rtsp_server::RTSPAuth::new();

        // Create token for authenticated users
        let token = gstreamer_rtsp_server::RTSPToken::new_empty();

        // Add basic auth credentials
        let basic = gstreamer_rtsp_server::RTSPAuth::make_basic(username, password);
        auth.add_basic(&basic, &token);

        // Set auth on server
        self.server.set_auth(Some(&auth));

        debug!("Authentication configured");
        Ok(())
    }

    /// Stop the RTSP server
    pub fn stop(&self) {
        self.main_loop.quit();
        info!("RTSP server stopped");
    }
}
