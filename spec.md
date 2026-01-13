# Universal Restreamer Specification

## Overview

A GStreamer-based restreamer that accepts multiple input source types and outputs them as RTSP streams. Each configured source becomes available at `rtsp://host:port/<name>/stream`.

## Input Sources

| Type | GStreamer Elements | Notes |
|------|-------------------|-------|
| V4L2 | `v4l2src` -> `x264enc` | Raw video, needs encoding |
| RTSP | `rtspsrc` -> `rtph264depay` | Usually already H.264 |
| RTMP | `rtmpsrc` -> `flvdemux` | H.264 in FLV container |
| SRT  | `srtsrc` -> `tsdemux` | MPEG-TS, usually H.264 |

## Architecture

```
                          Restreamer
   +----------------------------------------------------------+
   |                                                          |
   |   Config (TOML)                                          |
   |        |                                                 |
   |        v                                                 |
   |   +---------+    +---------+    +---------+              |
   |   |  V4L2   |    |  RTSP   |    |   SRT   |    ...       |
   |   | Pipeline|    | Pipeline|    | Pipeline|              |
   |   +----+----+    +----+----+    +----+----+              |
   |        |              |              |                   |
   |        v              v              v                   |
   |   +---------+    +---------+    +---------+              |
   |   | AppSink |    | AppSink |    | AppSink |              |
   |   +----+----+    +----+----+    +----+----+              |
   |        |              |              |                   |
   |        |      Channel (H.264 frames)                     |
   |        |              |              |                   |
   |        v              v              v                   |
   |   +------------------------------------------+           |
   |   |           RTSPServer :8554               |           |
   |   |  +--------+ +--------+ +--------+        |           |
   |   |  |/cam1/  | |/feed/  | |/srt1/  |        |           |
   |   |  |stream  | |stream  | |stream  |        |           |
   |   |  |AppSrc  | |AppSrc  | |AppSrc  |        |           |
   |   |  +--------+ +--------+ +--------+        |           |
   |   +------------------------------------------+           |
   +----------------------------------------------------------+
```

## Configuration Format (TOML)

```toml
[server]
rtsp_port = 8554
bind_address = "0.0.0.0"

# V4L2 source with software encoding (encoding required for raw video)
[[sources]]
name = "webcam"
type = "v4l2"
device = "/dev/video0"
width = 1280
height = 720
framerate = 30

[sources.encode]
bitrate = 2000000         # bits/sec
keyframe_interval = 60    # frames (key-int-max)
preset = "veryfast"       # x264 preset
tune = "zerolatency"      # x264 tune

[sources.auth]
enabled = false

# RTSP source - passthrough by default, can optionally re-encode
[[sources]]
name = "ipcam"
type = "rtsp"
url = "rtsp://192.168.1.100:554/stream1"
username = "admin"        # optional source auth
password = "password"
latency = 200             # ms, rtspsrc latency

[sources.auth]
enabled = true
username = "viewer"
password = "secret"

# RTSP source with transcoding (re-encode to different bitrate)
[[sources]]
name = "ipcam-lowres"
type = "rtsp"
url = "rtsp://192.168.1.100:554/stream1"
transcode = true          # decode and re-encode

[sources.encode]
bitrate = 500000
keyframe_interval = 30
preset = "ultrafast"
tune = "zerolatency"

# RTMP source - passthrough
[[sources]]
name = "obs-stream"
type = "rtmp"
url = "rtmp://localhost/live/stream"

# RTMP source with transcoding
[[sources]]
name = "obs-lowbitrate"
type = "rtmp"
url = "rtmp://localhost/live/stream"
transcode = true

[sources.encode]
bitrate = 1000000
keyframe_interval = 60
preset = "faster"

# SRT source (listener mode) - passthrough
[[sources]]
name = "srt-input"
type = "srt"
uri = "srt://0.0.0.0:9000?mode=listener"

# SRT with transcoding and passphrase
[[sources]]
name = "srt-transcoded"
type = "srt"
uri = "srt://0.0.0.0:9001?mode=listener"
passphrase = "minimum10chars"
transcode = true

[sources.encode]
bitrate = 3000000
keyframe_interval = 90
preset = "medium"
tune = "film"
```

## x264 Encoder Options

### Presets (speed vs quality)
- `ultrafast` - Fastest encoding, lowest quality
- `superfast`
- `veryfast`
- `faster`
- `fast`
- `medium` - Default balance
- `slow`
- `slower`
- `veryslow` - Slowest encoding, highest quality

### Tune Options
- `zerolatency` - Best for live streaming, minimal buffering
- `film` - High quality film content
- `animation` - Animated content
- `grain` - Preserve film grain
- `stillimage` - Slideshow/static content
- `fastdecode` - Optimized for fast decoding

## GStreamer Pipelines

### V4L2 (always encodes)
```
v4l2src device={device}
  ! video/x-raw,width={width},height={height},framerate={framerate}/1
  ! videoconvert
  ! x264enc bitrate={bitrate/1000} key-int-max={keyframe_interval}
            speed-preset={preset} tune={tune}
  ! video/x-h264,stream-format=byte-stream,alignment=au
  ! appsink name=sink emit-signals=true sync=false
```

### RTSP (passthrough)
```
rtspsrc location={url} latency={latency} user-id={user} user-pw={pass}
  ! rtph264depay
  ! h264parse
  ! video/x-h264,stream-format=byte-stream,alignment=au
  ! appsink name=sink emit-signals=true sync=false
```

### RTSP (transcode)
```
rtspsrc location={url} latency={latency} user-id={user} user-pw={pass}
  ! rtph264depay
  ! avdec_h264
  ! videoconvert
  ! x264enc bitrate={bitrate/1000} key-int-max={keyframe_interval}
            speed-preset={preset} tune={tune}
  ! video/x-h264,stream-format=byte-stream,alignment=au
  ! appsink name=sink emit-signals=true sync=false
```

### RTMP (passthrough)
```
rtmpsrc location={url}
  ! flvdemux name=demux
  demux.video
  ! h264parse
  ! video/x-h264,stream-format=byte-stream,alignment=au
  ! appsink name=sink emit-signals=true sync=false
```

### RTMP (transcode)
```
rtmpsrc location={url}
  ! flvdemux name=demux
  demux.video
  ! avdec_h264
  ! videoconvert
  ! x264enc bitrate={bitrate/1000} key-int-max={keyframe_interval}
            speed-preset={preset} tune={tune}
  ! video/x-h264,stream-format=byte-stream,alignment=au
  ! appsink name=sink emit-signals=true sync=false
```

### SRT (passthrough)
```
srtsrc uri={uri} [passphrase={passphrase}]
  ! tsdemux name=demux
  demux.
  ! h264parse
  ! video/x-h264,stream-format=byte-stream,alignment=au
  ! appsink name=sink emit-signals=true sync=false
```

### SRT (transcode)
```
srtsrc uri={uri} [passphrase={passphrase}]
  ! tsdemux name=demux
  demux.
  ! avdec_h264
  ! videoconvert
  ! x264enc bitrate={bitrate/1000} key-int-max={keyframe_interval}
            speed-preset={preset} tune={tune}
  ! video/x-h264,stream-format=byte-stream,alignment=au
  ! appsink name=sink emit-signals=true sync=false
```

### RTSP Output Factory
```
appsrc name=videosrc is-live=true format=time do-timestamp=true
       caps=video/x-h264,stream-format=byte-stream,alignment=au
  ! h264parse
  ! rtph264pay name=pay0 pt=96
```

## Usage

```bash
# Run with config file
simplertsp --config config.toml

# Run with default config path (./config.toml)
simplertsp
```

## Output URLs

Each source creates an RTSP endpoint at:
```
rtsp://<bind_address>:<rtsp_port>/<source_name>/stream
```

Example with config above:
- `rtsp://localhost:8554/webcam/stream`
- `rtsp://localhost:8554/ipcam/stream`
- `rtsp://localhost:8554/obs-stream/stream`
- `rtsp://localhost:8554/srt-input/stream`
