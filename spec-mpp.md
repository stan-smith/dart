# Rockchip MPP H.265 Hardware Encoder Integration — Spec

## Goal

Add runtime detection of Rockchip MPP GStreamer elements. When available, use `mpph265enc` for hardware H.265 encoding instead of `x264enc` H.264. The RTSP output adapts per-source to serve the correct codec.

No config changes required — detection is automatic.

## Background

On the Radxa Zero 3E (RK3566, 973MB RAM):
- `mpph265enc` does 1080p→1080p transcode at 1.9x realtime with **29s CPU** (vs minutes with x264)
- H.264 encode via `mpph264enc` + `videoconvert` OOM'd the board twice — **H.265 only**
- MPP handles NV12→encode natively via RGA — no `videoconvert` needed
- `mppvideodec` has `width`/`height` properties for RGA hardware downscaling

## 1. Codec Enum

Add to `config.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputCodec {
    H264,
    H265,
}
```

This is **not** a config field — it's determined at runtime and threaded through the system so the RTSP server knows which payloader to use.

## 2. MPP Detection

Add to `sources/mod.rs`:

```rust
/// Check if Rockchip MPP H.265 encoder is available
pub fn mpp_available() -> bool {
    gstreamer::ElementFactory::find("mpph265enc").is_some()
}
```

Call **once** at startup in `main.rs`, store the result, pass it into source creation and RTSP mount setup.

## 3. Pipeline Changes

### 3a. V4L2 Source (`sources/v4l2.rs`)

**When MPP is available:**
```
v4l2src device={device}
  ! video/x-raw,format=NV12,width={width},height={height},framerate={framerate}/1
  ! mpph265enc bps={bitrate_bps} gop={keyframe_interval}
  ! video/x-h265,stream-format=byte-stream,alignment=au
  ! h265parse
  ! video/x-h265,stream-format=byte-stream,alignment=au
  ! appsink name=sink emit-signals=true sync=false
```

Key differences from x264 path:
- **No `videoconvert`** — MPP accepts NV12 directly
- **No `videoscale`** — if downscaling is needed, use `mppvideodec` width/height properties (not applicable for V4L2 direct capture, but keep in mind)
- **NV12 caps on source** — explicit format negotiation since MPP expects NV12
- `bps` property is in **bits/sec** (multiply config kbps × 1000)
- `gop` property replaces `key-int-max`

**When MPP is NOT available** (existing path, unchanged):
```
v4l2src device={device}{source_caps}
  ! videoconvert ! videoscale ! {output_caps}
  ! x264enc bitrate={kbps} key-int-max={ki} speed-preset={preset} tune={tune}
  ! video/x-h264,stream-format=byte-stream,alignment=au
  ! h264parse
  ! video/x-h264,stream-format=byte-stream,alignment=au
  ! appsink name=sink emit-signals=true sync=false
```

### 3b. RTSP Source — Passthrough (`sources/rtsp.rs`)

**No change.** Passthrough doesn't re-encode. The existing H.264 depay→parse→appsink path works.

Note: RTSP passthrough sources always output H.264 (since that's what most IP cameras send). The codec for the mount is `H264`.

### 3c. RTSP Source — Transcode (`sources/rtsp.rs`)

**When MPP is available:**
```
rtspsrc location="{url}" latency={latency} [user-id="{user}" user-pw="{pass}"]
  ! rtph264depay
  ! mppvideodec
  ! mpph265enc bps={bitrate_bps} gop={keyframe_interval}
  ! video/x-h265,stream-format=byte-stream,alignment=au
  ! h265parse
  ! video/x-h265,stream-format=byte-stream,alignment=au
  ! appsink name=sink emit-signals=true sync=false
```

Key: Uses `mppvideodec` (hardware decode) instead of `avdec_h264` (software decode). Full hardware pipeline, near-zero CPU.

**When MPP is NOT available** (existing path, unchanged):
```
rtspsrc ... ! rtph264depay ! avdec_h264
  ! videoconvert
  ! x264enc bitrate={kbps} key-int-max={ki} speed-preset={preset} tune={tune}
  ! video/x-h264 ... ! h264parse ! appsink
```

### 3d. Summary Table

| Source | MPP Available | Encoder | Output Codec | videoconvert? |
|--------|--------------|---------|-------------|---------------|
| V4L2 | Yes | mpph265enc | H.265 | No |
| V4L2 | No | x264enc | H.264 | Yes |
| RTSP passthrough | Either | None | H.264 | No |
| RTSP transcode | Yes | mppvideodec + mpph265enc | H.265 | No |
| RTSP transcode | No | avdec_h264 + x264enc | H.264 | Yes |

## 4. RTSP Server Output (`rtsp.rs`)

The `add_mount` method must accept the output codec and set the factory launch string accordingly.

**H.264 output (existing):**
```
( appsrc name=videosrc is-live=true format=time do-timestamp=true
    caps=video/x-h264,stream-format=byte-stream,alignment=au
  ! h264parse
  ! rtph264pay name=pay0 pt=96 )
```

**H.265 output (new):**
```
( appsrc name=videosrc is-live=true format=time do-timestamp=true
    caps=video/x-h265,stream-format=byte-stream,alignment=au
  ! h265parse
  ! rtph265pay name=pay0 pt=96 )
```

### Change to `add_mount` signature:

```rust
pub fn add_mount(
    &self,
    source: &SourceConfig,
    codec: OutputCodec,
) -> Result<Arc<Mutex<Option<FrameSender>>>>
```

## 5. Fallback Image (`fallback.rs`)

When MPP is available, encode the fallback image using H.265:

```
filesrc location="{path}"
  ! decodebin ! videoconvert ! videoscale
  ! video/x-raw,width=640,height=480
  ! mpph265enc gop=1
  ! video/x-h265,stream-format=byte-stream,alignment=au
  ! h265parse
  ! appsink name=sink emit-signals=false sync=false
```

Note: `videoconvert` is still needed here because `decodebin` may output formats MPP doesn't accept from an image decoder. This is a one-shot operation at startup so CPU cost is irrelevant.

The `FallbackFrame::from_image` method needs an `mpp_available: bool` parameter.

## 6. Main.rs Changes

```rust
fn main() -> Result<()> {
    // ... existing init ...

    gstreamer::init()?;

    // Detect MPP support once
    let mpp = sources::mpp_available();
    if mpp {
        info!("Rockchip MPP detected — using hardware H.265 encoding");
    } else {
        info!("MPP not available — using software x264 H.264 encoding");
    }

    // For each source:
    let codec = match source_config.source_type {
        SourceType::V4l2 => {
            if mpp { OutputCodec::H265 } else { OutputCodec::H264 }
        }
        SourceType::Rtsp => {
            if source_config.transcode && mpp {
                OutputCodec::H265
            } else {
                OutputCodec::H264  // passthrough is always H.264
            }
        }
    };

    let frame_tx = rtsp_server.add_mount(&source_config, codec)?;

    // Fallback also needs codec awareness
    let fallback = FallbackFrame::from_image(path, mpp)?;

    // Source needs to know about MPP for pipeline creation
    let source = Source::new(source_config, frame_tx, fallback, mpp)?;
}
```

## 7. Source Creation (`sources/mod.rs`)

Add `mpp: bool` field to `Source`:

```rust
pub struct Source {
    name: String,
    config: SourceConfig,
    frame_tx: Arc<Mutex<Option<FrameSender>>>,
    fallback: Option<FallbackFrame>,
    state: Arc<Mutex<SourceState>>,
    running: Arc<AtomicBool>,
    mpp: bool,  // NEW
}
```

Pass `mpp` through to `create_and_run_pipeline` → `v4l2::create_pipeline` / `rtsp::create_pipeline`.

Update pipeline creation signatures:
```rust
// v4l2.rs
pub fn create_pipeline(config: &SourceConfig, mpp: bool) -> Result<gstreamer::Pipeline>

// rtsp.rs
pub fn create_pipeline(config: &SourceConfig, mpp: bool) -> Result<gstreamer::Pipeline>
```

## 8. Encoder Helper Updates (`sources/mod.rs`)

The existing `build_encoder_string` only handles x264. Add MPP equivalent:

```rust
pub fn build_mpp_h265_encoder_string(encode: &EncodeConfig) -> String {
    format!(
        "mpph265enc bps={} gop={}",
        encode.bitrate * 1000,  // config is kbps, MPP wants bps
        encode.keyframe_interval,
    )
}
```

Update `h264_caps()` to have an H.265 equivalent:

```rust
pub fn h265_caps() -> &'static str {
    "video/x-h265,stream-format=byte-stream,alignment=au"
}
```

## 9. Files Changed

| File | Changes |
|------|---------|
| `src/config.rs` | Add `OutputCodec` enum |
| `src/main.rs` | MPP detection at startup, pass `mpp` and `codec` through |
| `src/rtsp.rs` | `add_mount` takes `OutputCodec`, dynamic factory launch string |
| `src/sources/mod.rs` | Add `mpp_available()`, `mpp` field on Source, `build_mpp_h265_encoder_string()`, `h265_caps()` |
| `src/sources/v4l2.rs` | MPP pipeline branch (NV12, no videoconvert, mpph265enc) |
| `src/sources/rtsp.rs` | MPP transcode branch (mppvideodec + mpph265enc) |
| `src/fallback.rs` | MPP-aware encoding (mpph265enc when available) |

## 10. Non-Goals

- No config schema changes (auto-detect only)
- No H.264 MPP support (buggy on RK3566, OOMs with videoconvert)
- No RGA standalone downscaling element (not exposed in GStreamer on this board)
- No changes to RTSP passthrough path
- No changes to config wizard (future work)

## 11. Testing

**Acceptance criteria**: From the dev machine, successfully run:
```bash
ffprobe rtsp://radxa-zero3:8554/<name>/stream
```

And see H.265 codec in the output.

**On a non-MPP machine**: Dart should behave identically to today (x264 H.264).
