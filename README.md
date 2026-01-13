# Dart

Point a camera at something. Get an RTSP stream out the other end.

[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL%203.0-blue.svg)](LICENSE)

---

## The Problem

Hardware encoders are expensive. A dedicated box that takes HDMI in and spits RTSP out will set you back £800+. They work fine, but when you've got a pile of cheap capture dongles and a spare Pi lying around, it feels like overkill.

Enter Dart.

## What is Dart?

Dart is a tiny, focused utility that takes video in and streams it out as RTSP. That's it. No frills, no bloat, no £800 price tag.

It's part of the [SlingShot](https://github.com/stan-smith/SlingShot) family of tools, but it stands on its own.

## Inputs

- **HDMI capture cards** — Any V4L2 device. Cheap USB capture dongles work fine.
- **Webcams** — Same deal. If Linux sees it, Dart can use it.
- **RTSP streams** — Transcode and re-stream existing IP cameras.

## Outputs

- **RTSP** — Connect your VMS, your NVR, your SlingShot instance, whatever.

## Quick Start

```bash
# HDMI capture card on /dev/video0
dart --config config.hdmi.toml

# Webcam
dart --config config.webcam.toml

# Re-stream an existing RTSP source
dart --config config.rtsp.toml
```

## Configuration

Dart uses TOML config files. Here's an example for an HDMI capture card:

```toml
[server]
rtsp_port = 8554
bind_address = "0.0.0.0"

[[sources]]
name = "hdmi"
type = "v4l2"
device = "/dev/video0"
format = "UYVY"           # For HDMI capture cards (TC358743, etc.)
width = 1280
height = 720
framerate = 30

[sources.encode]
bitrate = 2000            # kbps
keyframe_interval = 30
preset = "ultrafast"
tune = "zerolatency"
```

For a standard webcam, you can omit the `format` field:

```toml
[[sources]]
name = "webcam"
type = "v4l2"
device = "/dev/video0"
width = 1920
height = 1080
framerate = 30

[sources.encode]
bitrate = 4000
```

For relaying an existing RTSP stream:

```toml
[[sources]]
name = "camera"
type = "rtsp"
url = "rtsp://192.168.1.100:554/stream1"
latency = 200
fallback = "/path/to/fallback.jpg"
```

## Why GStreamer?

Because it works. Because it's battle-tested. I know how to make RTSP servers from SlingShot, so this is a no brainer.

Dart wraps GStreamer and handles all the pipeline nonsense so you don't have to.

## Hardware Requirements

If it can run Linux and has a USB port, it can probably run Dart.

Tested on:
- Raspberry Pi 4 (1080p/25fps, no sweat)
- Random x86 mini PCs
- Actual servers (overkill, but sure)

## Part of the Family

Dart is a focused breakout from [SlingShot](https://github.com/stan-smith/SlingShot). If you need:

- **Ultra-low bandwidth streaming over QUIC** → SlingShot
- **Simple HDMI/V4L2 to RTSP** → You're in the right place

## Contributing

This is early days. It works for my use case, but your mileage may vary.

Found a bug? Open an issue. Got a fix? PR welcome.

## Licence

AGPL-3.0. See [LICENSE](LICENSE).