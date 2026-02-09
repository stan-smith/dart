use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Main configuration structure
#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
}

/// Server configuration
#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_rtsp_port")]
    pub rtsp_port: u16,
    #[serde(default = "default_bind_address")]
    pub bind_address: String,
}

fn default_rtsp_port() -> u16 {
    8554
}

fn default_bind_address() -> String {
    "0.0.0.0".to_string()
}

/// Source configuration - represents one input stream
#[derive(Debug, Deserialize)]
pub struct SourceConfig {
    /// Unique name for this source (used in RTSP path)
    pub name: String,
    /// Source type: v4l2, rtsp
    #[serde(rename = "type")]
    pub source_type: SourceType,

    // V4L2 specific
    pub device: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub framerate: Option<u32>,
    /// Pixel format (e.g., "UYVY", "RGB3") - for capture cards that need explicit format
    pub format: Option<String>,

    // RTSP specific
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub latency: Option<u32>,

    // Transcoding
    #[serde(default)]
    pub transcode: bool,

    // Encoding settings (for V4L2 or when transcode=true)
    pub encode: Option<EncodeConfig>,

    // Output authentication
    pub auth: Option<AuthConfig>,

    /// Path to fallback image (shown when source disconnects)
    pub fallback: Option<String>,

    /// Reconnect interval in seconds (default: 10)
    #[serde(default = "default_reconnect_interval")]
    pub reconnect_interval: u64,
}

fn default_reconnect_interval() -> u64 {
    10
}

/// Source type enum
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceType {
    V4l2,
    Rtsp,
}

/// Output codec — determined at runtime based on MPP availability
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputCodec {
    H264,
    H265,
}

/// Encoding configuration
#[derive(Debug, Deserialize, Clone)]
pub struct EncodeConfig {
    /// Bitrate in kbps
    #[serde(default = "default_bitrate")]
    pub bitrate: u32,
    /// Keyframe interval in frames
    #[serde(default = "default_keyframe_interval")]
    pub keyframe_interval: u32,
    /// x264 preset
    #[serde(default = "default_preset")]
    pub preset: String,
    /// x264 tune option
    #[serde(default = "default_tune")]
    pub tune: String,
}

fn default_bitrate() -> u32 {
    2000 // 2 Mbps in kbps
}

fn default_keyframe_interval() -> u32 {
    60
}

fn default_preset() -> String {
    "veryfast".to_string()
}

fn default_tune() -> String {
    "zerolatency".to_string()
}

impl Default for EncodeConfig {
    fn default() -> Self {
        Self {
            bitrate: default_bitrate(),
            keyframe_interval: default_keyframe_interval(),
            preset: default_preset(),
            tune: default_tune(),
        }
    }
}

/// Authentication configuration for RTSP output
#[derive(Debug, Deserialize, Clone)]
pub struct AuthConfig {
    #[serde(default)]
    pub enabled: bool,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl Config {
    /// Load configuration from a TOML file
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;

        let config: Config = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))?;

        config.validate()?;
        Ok(config)
    }

    /// Validate the configuration
    fn validate(&self) -> Result<()> {
        for source in &self.sources {
            source.validate()?;
        }
        Ok(())
    }
}

impl SourceConfig {
    /// Validate source configuration
    fn validate(&self) -> Result<()> {
        // Validate name (alphanumeric, dash, underscore, start with alphanumeric)
        if self.name.is_empty() || self.name.len() > 32 {
            anyhow::bail!("Source name must be 1-32 characters: '{}'", self.name);
        }
        if !self
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            anyhow::bail!(
                "Source name must contain only alphanumeric, dash, underscore: '{}'",
                self.name
            );
        }
        if !self
            .name
            .chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric())
            .unwrap_or(false)
        {
            anyhow::bail!(
                "Source name must start with alphanumeric character: '{}'",
                self.name
            );
        }

        match self.source_type {
            SourceType::V4l2 => {
                if self.device.is_none() {
                    anyhow::bail!("V4L2 source '{}' requires 'device' field", self.name);
                }
                if self.encode.is_none() {
                    anyhow::bail!(
                        "V4L2 source '{}' requires 'encode' settings (raw video must be encoded)",
                        self.name
                    );
                }
            }
            SourceType::Rtsp => {
                if self.url.is_none() {
                    anyhow::bail!("RTSP source '{}' requires 'url' field", self.name);
                }
                if self.transcode && self.encode.is_none() {
                    anyhow::bail!(
                        "RTSP source '{}' has transcode=true but no 'encode' settings",
                        self.name
                    );
                }
            }
        }

        Ok(())
    }

    /// Get encoding config, using defaults if not specified
    pub fn encode_config(&self) -> EncodeConfig {
        self.encode.clone().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
            [server]
            rtsp_port = 8554

            [[sources]]
            name = "cam1"
            type = "v4l2"
            device = "/dev/video0"

            [sources.encode]
            bitrate = 2000000
        "#;

        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.server.rtsp_port, 8554);
        assert_eq!(config.sources.len(), 1);
        assert_eq!(config.sources[0].name, "cam1");
    }

    #[test]
    fn test_invalid_name() {
        let source = SourceConfig {
            name: "../bad".to_string(),
            source_type: SourceType::V4l2,
            device: Some("/dev/video0".to_string()),
            width: None,
            height: None,
            framerate: None,
            format: None,
            url: None,
            username: None,
            password: None,
            latency: None,
            transcode: false,
            encode: Some(EncodeConfig::default()),
            auth: None,
            fallback: None,
            reconnect_interval: 10,
        };
        assert!(source.validate().is_err());
    }
}
