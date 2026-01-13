//! Interactive configuration wizard

use anyhow::{Context, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Source type selection
#[derive(Debug, Clone, Copy)]
pub enum SourceType {
    V4l2,
    Rtsp,
}

/// Collected V4L2 configuration
#[derive(Debug)]
struct V4l2Config {
    name: String,
    device: String,
    format: Option<String>, // Only set for HDMI capture cards that need explicit format
    width: u32,
    height: u32,
    framerate: u32,
    bitrate: u32,
}

/// Collected RTSP configuration
#[derive(Debug)]
struct RtspConfig {
    name: String,
    url: String,
    username: Option<String>,
    password: Option<String>,
    latency: u32,
    transcode: bool,
    bitrate: Option<u32>, // Only if transcoding
}

/// V4L2 device info from v4l2-ctl --list-devices
#[derive(Debug, Clone)]
struct V4l2Device {
    name: String,
    path: String, // Primary video device path (first /dev/videoX)
}

/// V4L2 format info from v4l2-ctl
#[derive(Debug, Clone)]
struct V4l2Format {
    fourcc: String,
    description: String,
    resolutions: Vec<V4l2Resolution>,
}

/// V4L2 resolution with framerates
#[derive(Debug, Clone)]
struct V4l2Resolution {
    width: u32,
    height: u32,
    framerates: Vec<u32>,
}

/// Run the configuration wizard
pub fn run(output_path: &Path) -> Result<()> {
    println!("\nDart Configuration Wizard\n");
    println!("Made with love by Stan\n");

    let source_type = ask_source_type()?;

    let config_content = match source_type {
        SourceType::V4l2 => {
            let v4l2_config = v4l2_questions()?;
            generate_v4l2_config(&v4l2_config)
        }
        SourceType::Rtsp => {
            let rtsp_config = rtsp_questions()?;
            generate_rtsp_config(&rtsp_config)
        }
    };

    // Write config file
    fs::write(output_path, &config_content)
        .with_context(|| format!("Failed to write config to {}", output_path.display()))?;

    println!("\nConfig written to: {}", output_path.display());
    Ok(())
}

/// Ask user to select source type
fn ask_source_type() -> Result<SourceType> {
    let options = vec!["V4L2 (webcam, HDMI capture card)", "RTSP (IP camera, network stream)"];

    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("What type of video source are you using?")
        .items(&options)
        .default(0)
        .interact()?;

    Ok(match selection {
        0 => SourceType::V4l2,
        1 => SourceType::Rtsp,
        _ => unreachable!(),
    })
}

fn v4l2_questions() -> Result<V4l2Config> {
    // List available devices
    println!("Scanning for V4L2 devices...\n");
    let devices = list_v4l2_devices()?;

    if devices.is_empty() {
        anyhow::bail!("No V4L2 devices found. Is a camera connected?");
    }

    // Show device selector
    let device_options: Vec<String> = devices
        .iter()
        .map(|d| format!("{} ({})", d.name, d.path))
        .collect();

    let device_idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select video device")
        .items(&device_options)
        .default(0)
        .interact()?;

    let selected_device = &devices[device_idx];
    let device = selected_device.path.clone();

    // Default stream name from device name (lowercase, no spaces)
    let default_name = selected_device
        .name
        .to_lowercase()
        .split_whitespace()
        .next()
        .unwrap_or("camera")
        .to_string();

    // Ask for stream name
    let name: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Enter a name for this stream (used in RTSP URL)")
        .default(default_name)
        .interact_text()?;

    println!("\nProbing device capabilities...\n");

    let formats = probe_v4l2_device(&device)?;

    if formats.is_empty() {
        anyhow::bail!("No formats detected. Device may not be available.");
    }

    // Show available formats and let user choose
    let format_options: Vec<String> = formats
        .iter()
        .map(|f| format!("{} ({})", f.fourcc, f.description))
        .collect();

    let format_idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select video format")
        .items(&format_options)
        .default(0)
        .interact()?;

    let selected_format = &formats[format_idx];

    // Show available resolutions for selected format
    let resolution_options: Vec<String> = selected_format
        .resolutions
        .iter()
        .map(|r| {
            let fps_str = r
                .framerates
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join("/");
            format!("{}x{} @ {} fps", r.width, r.height, fps_str)
        })
        .collect();

    let res_idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select resolution")
        .items(&resolution_options)
        .default(0)
        .interact()?;

    let selected_res = &selected_format.resolutions[res_idx];

    // Select framerate if multiple available
    let framerate = if selected_res.framerates.len() > 1 {
        let fps_options: Vec<String> = selected_res
            .framerates
            .iter()
            .map(|f| format!("{} fps", f))
            .collect();

        let fps_idx = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Select framerate")
            .items(&fps_options)
            .default(0)
            .interact()?;

        selected_res.framerates[fps_idx]
    } else {
        selected_res.framerates[0]
    };

    // Ask for bitrate
    let bitrate: u32 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Enter encoding bitrate in kbps")
        .default(2000)
        .interact_text()?;

    println!("\nSelected configuration:");
    println!("  Name: {}", name);
    println!("  Device: {}", device);
    println!("  Resolution: {}x{}", selected_res.width, selected_res.height);
    println!("  Framerate: {} fps", framerate);
    println!("  Bitrate: {} kbps", bitrate);

    Ok(V4l2Config {
        name,
        device,
        format: None, // Let GStreamer auto-negotiate format
        width: selected_res.width,
        height: selected_res.height,
        framerate,
        bitrate,
    })
}

/// Generate TOML config content for V4L2 source
fn generate_v4l2_config(config: &V4l2Config) -> String {
    // Only include format if explicitly set (e.g., for HDMI capture cards)
    // Otherwise let GStreamer auto-negotiate
    let format_line = config
        .format
        .as_ref()
        .map(|f| format!("format = \"{}\"\n", f))
        .unwrap_or_default();

    format!(
        r#"[server]
rtsp_port = 8554
bind_address = "0.0.0.0"

[[sources]]
name = "{name}"
type = "v4l2"
device = "{device}"
{format_line}width = {width}
height = {height}
framerate = {framerate}

[sources.encode]
bitrate = {bitrate}
preset = "veryfast"
tune = "zerolatency"
"#,
        name = config.name,
        device = config.device,
        format_line = format_line,
        width = config.width,
        height = config.height,
        framerate = config.framerate,
        bitrate = config.bitrate,
    )
}

fn rtsp_questions() -> Result<RtspConfig> {
    // Ask for RTSP URL
    let url: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Enter the RTSP URL")
        .interact_text()?;

    println!("\nProbing stream with ffprobe...\n");

    let stream_info = probe_rtsp_stream(&url)?;

    println!("Detected stream:");
    println!("  Codec: {}", stream_info.codec);
    println!("  Resolution: {}x{}", stream_info.width, stream_info.height);
    if let Some(fps) = stream_info.framerate {
        println!("  Framerate: {} fps", fps);
    }

    // Default name from URL (extract hostname or path)
    let default_name = url
        .split("://")
        .nth(1)
        .and_then(|s| s.split('/').next())
        .and_then(|s| s.split(':').next())
        .and_then(|s| s.split('@').last())
        .unwrap_or("camera")
        .replace('.', "-");

    let name: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Enter a name for this stream (used in RTSP URL)")
        .default(default_name)
        .interact_text()?;

    // Ask about transcoding
    let transcode = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Re-encode the stream? (say No for passthrough)")
        .default(false)
        .interact()?;

    let bitrate = if transcode {
        Some(
            Input::with_theme(&ColorfulTheme::default())
                .with_prompt("Enter encoding bitrate in kbps")
                .default(2000u32)
                .interact_text()?,
        )
    } else {
        None
    };

    println!("\nSelected configuration:");
    println!("  Name: {}", name);
    println!("  URL: {}", url);
    println!("  Mode: {}", if transcode { "transcode" } else { "passthrough" });
    if let Some(br) = bitrate {
        println!("  Bitrate: {} kbps", br);
    }

    Ok(RtspConfig {
        name,
        url,
        username: None,
        password: None,
        latency: 200,
        transcode,
        bitrate,
    })
}

/// Stream info from ffprobe
#[derive(Debug)]
struct RtspStreamInfo {
    codec: String,
    width: u32,
    height: u32,
    framerate: Option<u32>,
}

/// Probe RTSP stream using ffprobe
fn probe_rtsp_stream(url: &str) -> Result<RtspStreamInfo> {
    let output = Command::new("ffprobe")
        .args([
            "-v", "quiet",
            "-select_streams", "v:0",
            "-show_entries", "stream=codec_name,width,height,r_frame_rate",
            "-of", "csv=p=0",
            "-rtsp_transport", "tcp",
            url,
        ])
        .output()
        .context("Failed to run ffprobe. Is ffmpeg installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ffprobe failed: {}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.trim().split(',').collect();

    if parts.len() < 3 {
        anyhow::bail!("Could not detect stream info. Is the URL correct?");
    }

    let codec = parts[0].to_string();
    let width: u32 = parts[1].parse().unwrap_or(0);
    let height: u32 = parts[2].parse().unwrap_or(0);

    // Parse framerate (format: "30/1" or "30000/1001")
    let framerate = parts.get(3).and_then(|fps| {
        let fps_parts: Vec<&str> = fps.split('/').collect();
        if fps_parts.len() == 2 {
            let num: f64 = fps_parts[0].parse().ok()?;
            let den: f64 = fps_parts[1].parse().ok()?;
            if den > 0.0 {
                return Some((num / den).round() as u32);
            }
        }
        None
    });

    Ok(RtspStreamInfo {
        codec,
        width,
        height,
        framerate,
    })
}

/// Generate TOML config content for RTSP source
fn generate_rtsp_config(config: &RtspConfig) -> String {
    let mut source_config = format!(
        r#"[server]
rtsp_port = 8554
bind_address = "0.0.0.0"

[[sources]]
name = "{name}"
type = "rtsp"
url = "{url}"
latency = {latency}
"#,
        name = config.name,
        url = config.url,
        latency = config.latency,
    );

    if config.transcode {
        source_config.push_str(&format!(
            r#"transcode = true

[sources.encode]
bitrate = {}
preset = "veryfast"
tune = "zerolatency"
"#,
            config.bitrate.unwrap_or(2000)
        ));
    }

    source_config
}

/// Probe V4L2 device capabilities using v4l2-ctl
fn probe_v4l2_device(device: &str) -> Result<Vec<V4l2Format>> {
    let output = Command::new("v4l2-ctl")
        .args(["-d", device, "--list-formats-ext"])
        .output()
        .context("Failed to run v4l2-ctl. Is v4l-utils installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("v4l2-ctl failed: {}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_v4l2_formats(&stdout)
}

/// Parse v4l2-ctl --list-formats-ext output
fn parse_v4l2_formats(output: &str) -> Result<Vec<V4l2Format>> {
    let mut formats: Vec<V4l2Format> = Vec::new();
    let mut current_format: Option<V4l2Format> = None;
    let mut current_resolution: Option<V4l2Resolution> = None;

    for line in output.lines() {
        let trimmed = line.trim();

        // Match format line: [0]: 'YUYV' (YUYV 4:2:2)
        if trimmed.starts_with('[') && trimmed.contains("'") {
            // Save previous format if exists
            if let Some(mut fmt) = current_format.take() {
                if let Some(res) = current_resolution.take() {
                    fmt.resolutions.push(res);
                }
                formats.push(fmt);
            }

            // Parse new format
            if let Some(fourcc) = extract_fourcc(trimmed) {
                let description = extract_description(trimmed).unwrap_or_default();
                current_format = Some(V4l2Format {
                    fourcc,
                    description,
                    resolutions: Vec::new(),
                });
            }
        }
        // Match resolution line: Size: Discrete 1920x1080
        else if trimmed.starts_with("Size: Discrete") {
            // Save previous resolution if exists
            if let Some(fmt) = current_format.as_mut() {
                if let Some(res) = current_resolution.take() {
                    fmt.resolutions.push(res);
                }
            }

            // Parse new resolution
            if let Some((w, h)) = extract_resolution(trimmed) {
                current_resolution = Some(V4l2Resolution {
                    width: w,
                    height: h,
                    framerates: Vec::new(),
                });
            }
        }
        // Match framerate line: Interval: Discrete 0.033s (30.000 fps)
        else if trimmed.starts_with("Interval: Discrete") {
            if let Some(fps) = extract_framerate(trimmed) {
                if let Some(res) = current_resolution.as_mut() {
                    if !res.framerates.contains(&fps) {
                        res.framerates.push(fps);
                    }
                }
            }
        }
    }

    // Don't forget the last format/resolution
    if let Some(mut fmt) = current_format {
        if let Some(res) = current_resolution {
            fmt.resolutions.push(res);
        }
        formats.push(fmt);
    }

    Ok(formats)
}

/// Extract FOURCC code from format line like "[0]: 'YUYV' (YUYV 4:2:2)"
fn extract_fourcc(line: &str) -> Option<String> {
    let start = line.find('\'')?;
    let end = line[start + 1..].find('\'')?;
    Some(line[start + 1..start + 1 + end].to_string())
}

/// Extract description from format line like "[0]: 'YUYV' (YUYV 4:2:2)"
fn extract_description(line: &str) -> Option<String> {
    let start = line.find('(')?;
    let end = line.rfind(')')?;
    if start < end {
        Some(line[start + 1..end].to_string())
    } else {
        None
    }
}

/// Extract resolution from line like "Size: Discrete 1920x1080"
fn extract_resolution(line: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    for part in parts {
        if part.contains('x') {
            let dims: Vec<&str> = part.split('x').collect();
            if dims.len() == 2 {
                let w = dims[0].parse().ok()?;
                let h = dims[1].parse().ok()?;
                return Some((w, h));
            }
        }
    }
    None
}

/// Extract framerate from line like "Interval: Discrete 0.033s (30.000 fps)"
fn extract_framerate(line: &str) -> Option<u32> {
    // Look for (XX.XXX fps) pattern
    if let Some(start) = line.find('(') {
        if let Some(end) = line.find(" fps)") {
            let fps_str = &line[start + 1..end];
            if let Ok(fps) = fps_str.parse::<f64>() {
                return Some(fps.round() as u32);
            }
        }
    }
    None
}

/// List available V4L2 devices using v4l2-ctl --list-devices
fn list_v4l2_devices() -> Result<Vec<V4l2Device>> {
    let output = Command::new("v4l2-ctl")
        .arg("--list-devices")
        .output()
        .context("Failed to run v4l2-ctl. Is v4l-utils installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("v4l2-ctl failed: {}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_v4l2_devices(&stdout))
}

/// Parse v4l2-ctl --list-devices output
/// Format:
/// Device Name (bus info):
///     /dev/video0
///     /dev/video1
///     /dev/media0
fn parse_v4l2_devices(output: &str) -> Vec<V4l2Device> {
    let mut devices = Vec::new();
    let mut current_name: Option<String> = None;

    for line in output.lines() {
        if !line.starts_with('\t') && !line.starts_with(' ') && line.contains(':') {
            // Device name line - extract name before the parenthesis or colon
            let name = line
                .split('(')
                .next()
                .unwrap_or(line)
                .split(':')
                .next()
                .unwrap_or(line)
                .trim()
                .to_string();
            current_name = Some(name);
        } else if let Some(name) = &current_name {
            let path = line.trim();
            // Only include /dev/videoX devices (not /dev/mediaX)
            if path.starts_with("/dev/video") {
                devices.push(V4l2Device {
                    name: name.clone(),
                    path: path.to_string(),
                });
                // Only take the first video device for each name
                current_name = None;
            }
        }
    }

    devices
}