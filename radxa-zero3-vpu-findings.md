# Radxa Zero 3E - VPU Hardware Transcoding Findings

## Connection
- **SSH**: `ssh radxa@radxa-zero3`
- **Tailscale IP**: 100.117.5.78
- **Tailscale hostname**: `radxa-zero3.tail3016c.ts.net`
- **User/Pass**: radxa / radxa (SSH key installed from stan's laptop)

## Hardware
- **Board**: Radxa ZERO 3E (Rockchip RK3566)
- **RAM**: 973MB
- **OS**: Debian 12 (Bookworm), kernel 6.1.84-10-rk2410
- **VPU devices**: `/dev/video-dec0`, `/dev/video-enc0`, `/dev/mpp_service`
- **2D accel**: RGA (`librga2`), used automatically by MPP for buffer conversion

## Software Stack
- **GStreamer**: 1.22.9 with `gstreamer1.0-rockchip1` plugin (v1.14.4)
- **Rockchip MPP**: `librockchip-mpp1` 1.5.0-1

### Available GStreamer HW Elements
| Element | Function |
|---|---|
| `mppvideodec` | HW video decoder (H.264, H.265, VP8, VP9, MPEG2) |
| `mpph265enc` | HW H.265 encoder |
| `mpph264enc` | HW H.264 encoder |
| `mppjpegenc` | HW JPEG encoder |
| `mppjpegdec` | HW JPEG decoder |
| `rkximagesink` | Direct display output |

## Key Finding: Use Rockchip MPP, NOT Mainline V4L2

The board shipped with two driver stacks. The vendor MPP stack is dramatically better:

| | Rockchip MPP (`mppvideodec`) | Mainline V4L2 (`v4l2slh264dec`) |
|---|---|---|
| 1080p H.264 decode (10min) | **52s** | 3m58s |
| CPU usage during decode | Low | High (software-like) |
| HW transcode | Works (RGA buffer passing) | Crashes board (OOM) |
| Zero-copy decode→encode | Yes (via MPP + RGA) | No (needs `videoconvert`) |

## Benchmark Results

### Test file: Big Buck Bunny 1080p H.265 (9m56s, 14315 frames)

### Decode Only
| Decoder | Wall Clock | CPU User | Notes |
|---|---|---|---|
| `mppvideodec` (HW) | **5.7s** | 1.7s | ~2500 fps, CPU idle |
| `avdec_h265` (SW) | 2m55s | 9m21s | Maxes all cores |

### Full Transcode (H.265 1080p input)
| Pipeline | Wall Clock | CPU User | Output | Speed |
|---|---|---|---|---|
| 1080p → 1080p H.265 | **5m16s** | **29s** | 426MB | 1.9x realtime |
| 1080p → 480p H.265 (videoscale) | 4m04s | 4m07s | 86MB | 2.4x realtime |
| 1080p → 480p H.265 (RGA scale) | **1m42s** | **27s** | 86MB | **5.8x realtime** |

### Working GStreamer Pipelines

**Decode only:**
```bash
gst-launch-1.0 filesrc location=input.mp4 ! qtdemux ! h265parse ! mppvideodec ! fakesink sync=false
```

**1080p → 1080p H.265 transcode (fastest, near-zero CPU):**
```bash
gst-launch-1.0 filesrc location=input.mp4 ! qtdemux name=d d.video_0 \
  ! h265parse ! mppvideodec ! mpph265enc ! h265parse \
  ! mp4mux ! filesink location=output.mp4
```

**1080p → 480p H.265 transcode (RGA hardware downscale — recommended):**
```bash
gst-launch-1.0 filesrc location=input.mp4 ! qtdemux name=d d.video_0 \
  ! h265parse ! mppvideodec width=854 height=480 \
  ! mpph265enc ! h265parse \
  ! mp4mux ! filesink location=output.mp4
```

**1080p → 480p H.265 transcode (software downscale — slow):**
```bash
gst-launch-1.0 filesrc location=input.mp4 ! qtdemux name=d d.video_0 \
  ! h265parse ! mppvideodec \
  ! videoscale ! video/x-raw,width=854,height=480 \
  ! mpph265enc ! h265parse \
  ! mp4mux ! filesink location=output.mp4
```

## Notes and Gotchas

1. **H.265 encode is the way forward** — H.264 encode with `videoconvert` OOM'd the 1GB board twice
2. **MPP + RGA handles buffer passing** — prints `rga_api version 1.10.1_[4]` when active, no `videoconvert` needed between decode and encode at same resolution
3. **Downscaling via RGA** — `mppvideodec` has built-in `width` and `height` properties that use RGA for hardware scaling. Use these instead of `videoscale` (5.8x vs 2.4x realtime)
4. **Memory is tight** — 973MB total, ~570MB available. Avoid chaining HW decode + software convert + HW encode at 1080p with H.264 path
5. **No RGA GStreamer element** — `librga2` is installed and MPP uses it internally, but there's no `rkrgafilter` or similar exposed as a standalone GStreamer element for scaling

## HDMI Capture (TC358743 via CSI-2)

### Setup
- **Bridge**: Suptronics X1300 HDMI to CSI-2 (Toshiba TC358743XBG)
- **Overlay**: `radxa-zero3-tc358743.dtbo` (enable in `/boot/dtbo/`, add `fdtoverlays` line to extlinux.conf)
- **Cable**: 22-pin to 22-pin FFC (RPi 5 cable). Contacts face **up** on Radxa side
- **Driver**: `tc35874x` (Rockchip variant, built into kernel as `CONFIG_VIDEO_TC35874X=y`)
- **Capture device**: `/dev/video0` (rkisp_mainpath), NV12 format

### Enabling the overlay
Add to `/boot/extlinux/extlinux.conf` under the `label l0` section:
```
	fdtoverlays /boot/dtbo/radxa-zero3-tc358743.dtbo
```

### Live Capture + H.265 Encode Results

| Pipeline | Realtime? | CPU user | Notes |
|---|---|---|---|
| 1080p30 → H.265 1080p | Yes | **1.1s** for 8s video | Essentially zero CPU |
| 1080p30 → H.265 480p (videoscale) | Yes | 12s for 22s video | Software downscale |

### Working Pipelines

**HDMI 1080p capture → H.265 file (recommended):**
```bash
gst-launch-1.0 v4l2src device=/dev/video0 \
  ! "video/x-raw,format=NV12,width=1920,height=1080,framerate=30/1" \
  ! mpph265enc ! h265parse ! mp4mux ! filesink location=output.mp4
```

**HDMI 1080p capture → downscale 480p → H.265 file:**
```bash
gst-launch-1.0 v4l2src device=/dev/video0 \
  ! "video/x-raw,format=NV12,width=1920,height=1080,framerate=30/1" \
  ! videoscale ! "video/x-raw,width=854,height=480" \
  ! mpph265enc ! h265parse ! mp4mux ! filesink location=output.mp4
```

## TODO
- [x] RGA-accelerated scaling — use `mppvideodec width=W height=H` properties
- [ ] Test `mpph264enc` directly without `videoconvert` (may work like H.265 path)
- [ ] Benchmark with audio passthrough (`-c:a copy` equivalent)
- [ ] Test streaming pipelines (RTSP/HLS output)
