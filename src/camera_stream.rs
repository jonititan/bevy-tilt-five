//! Headset camera streams: enables the SDK's onboard cameras (0 = tangible
//! tracking camera, 1 = head tracking camera), pumps filled frame buffers,
//! and writes greyscale pixels into Bevy `Image` handles the game owns.
//!
//! Enable a sink by spawning an entity with [`CameraStreamSink`]. The plugin
//! configures the stream on the next frame and updates the attached `Image`
//! as frames arrive. Despawning the entity disables the stream.

use bevy::{
    prelude::*,
    render::{
        render_asset::RenderAssetUsages,
        render_resource::{Extent3d, TextureDimension, TextureFormat},
        RenderApp,
    },
    utils::HashMap,
};
use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::bridge::{CameraFrame, Glasses};
use crate::plugin::{GlassesRenderList, RenderChannels};

/// Spawn an entity with this component to receive camera frames from one of
/// the glasses' onboard cameras.
#[derive(Component, Debug, Clone)]
pub struct CameraStreamSink {
    pub glasses: Glasses,
    /// 0 = tangible tracking (front), 1 = head tracking.
    pub camera_index: u8,
    /// Image the plugin should keep updated. Must be sized at least
    /// `T5_MIN_CAM_IMAGE_WIDTH × T5_MIN_CAM_IMAGE_HEIGHT`.
    pub image: Handle<Image>,
}

/// Public event emitted when a camera frame arrives. Consumers that want the
/// raw greyscale bytes (e.g. CV pipelines) can listen for this instead of
/// using the image sink.
#[derive(Event, Debug, Clone)]
pub struct HeadsetCameraFrame {
    pub glasses: Glasses,
    pub camera_index: u8,
    pub width: u16,
    pub height: u16,
    pub stride: u16,
    pub illumination_mode: u8,
    pub pos_cam_in_stage: Vec3,
    pub rot_stage_to_cam: Quat,
    pub pixels: Vec<u8>,
}

/// Entry in the SinkRegistry: track which (glasses, index) pairs have been
/// enabled with the SDK so we can skip repeated configure calls.
#[derive(Resource, Default)]
struct SinkRegistry(HashMap<(Glasses, u8), ()>);

#[derive(Resource)]
struct FrameChannel {
    rx: Receiver<CameraFrame>,
}

struct FrameChannelRender {
    tx: Sender<CameraFrame>,
}

#[derive(Resource, Default)]
struct ConfigureRequests {
    requests: Vec<(Glasses, u8, bool)>, // (glasses, camera_index, enable)
}

pub struct CameraStreamPlugin;

impl Plugin for CameraStreamPlugin {
    fn build(&self, app: &mut App) {
        let (tx, rx) = unbounded::<CameraFrame>();
        app.add_event::<HeadsetCameraFrame>()
            .insert_resource(FrameChannel { rx })
            .init_resource::<SinkRegistry>()
            .init_resource::<ConfigureRequests>()
            .add_systems(
                Update,
                (detect_new_sinks, drain_frames_main, detect_removed_sinks).chain(),
            );

        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.add_systems(ExtractSchedule, (apply_configure, drain_frames_render));
            render_app
                .world_mut()
                .insert_non_send_resource(FrameChannelRender { tx });
        }
    }
}

/// Watch for newly-spawned `CameraStreamSink` components and ask the render
/// side to enable the corresponding stream.
fn detect_new_sinks(
    sinks: Query<&CameraStreamSink, Added<CameraStreamSink>>,
    mut registry: ResMut<SinkRegistry>,
    mut requests: ResMut<ConfigureRequests>,
) {
    for sink in &sinks {
        let key = (sink.glasses.clone(), sink.camera_index);
        if registry.0.insert(key, ()).is_none() {
            requests
                .requests
                .push((sink.glasses.clone(), sink.camera_index, true));
        }
    }
}

/// When a sink is despawned, instruct the render side to disable the stream.
/// (Uses `RemovedComponents` because Added picks it up but we don't get a
/// despawn event per component directly in all Bevy versions.)
fn detect_removed_sinks(
    mut removed: RemovedComponents<CameraStreamSink>,
    mut registry: ResMut<SinkRegistry>,
    mut requests: ResMut<ConfigureRequests>,
    all: Query<&CameraStreamSink>,
) {
    if removed.is_empty() {
        return;
    }
    // Rebuild the set of still-live sinks and diff vs. registry.
    let mut still_live: HashMap<(Glasses, u8), ()> = HashMap::default();
    for sink in &all {
        still_live.insert((sink.glasses.clone(), sink.camera_index), ());
    }
    let mut to_remove = vec![];
    for key in registry.0.keys() {
        if !still_live.contains_key(key) {
            to_remove.push(key.clone());
        }
    }
    for key in to_remove {
        registry.0.remove(&key);
        requests.requests.push((key.0, key.1, false));
    }
    // consume the iterator so Bevy doesn't warn
    for _ in removed.read() {}
}

/// Render app: apply enable/disable requests sent from main.
fn apply_configure(
    mut channels: NonSendMut<RenderChannels>,
    mut requests: ResMut<ConfigureRequests>,
) {
    let drained: Vec<_> = requests.requests.drain(..).collect();
    for (gid, idx, enable) in drained {
        if let Err(e) = channels.client.configure_camera_stream(&gid, idx, enable) {
            warn!("configure_camera_stream({idx}, {enable}) on {gid}: {e}");
        }
    }
}

/// Render app: drain filled camera buffers from every connected pair of
/// glasses and ship them to the main app over the channel.
fn drain_frames_render(
    mut channels: NonSendMut<RenderChannels>,
    render_ch: NonSendMut<FrameChannelRender>,
    list: Res<GlassesRenderList>,
) {
    let ids: Vec<Glasses> = list.0.keys().cloned().collect();
    for gid in ids {
        match channels.client.drain_camera_frames(&gid) {
            Ok(frames) => {
                for f in frames {
                    let _ = render_ch.tx.send(f);
                    let _ = gid; // keep for log context
                }
            }
            Err(_) => {}
        }
    }
}

/// Main app: fire events + upload the latest frame per (glasses, index) into
/// the user-provided `Image`.
fn drain_frames_main(
    channel: Res<FrameChannel>,
    sinks: Query<&CameraStreamSink>,
    mut images: ResMut<Assets<Image>>,
    mut events: EventWriter<HeadsetCameraFrame>,
) {
    // Build index of sinks by (glasses_as_key_proxy_via_debug_string, camera_index).
    // Since Glasses isn't readily indexable without alloc, we iterate.
    let frames: Vec<CameraFrame> = channel.rx.try_iter().collect();
    for frame in frames {
        // Figure out which sinks match this frame.
        let mut matched_sink: Option<&CameraStreamSink> = None;
        for sink in &sinks {
            if sink.camera_index == frame.camera_index {
                // The frame doesn't carry a glasses id (SDK scopes it per-glasses
                // on the drain side). The render system *could* carry it along —
                // for now we pick the first sink matching the index. See TODO.
                matched_sink = Some(sink);
                break;
            }
        }

        events.send(HeadsetCameraFrame {
            glasses: matched_sink.map(|s| s.glasses.clone()).unwrap_or_else(|| Glasses::from("")),
            camera_index: frame.camera_index,
            width: frame.width,
            height: frame.height,
            stride: frame.stride,
            illumination_mode: frame.illumination_mode,
            pos_cam_in_stage: frame.pos_cam_in_stage,
            rot_stage_to_cam: frame.rot_stage_to_cam,
            pixels: frame.pixels.clone(),
        });

        if let Some(sink) = matched_sink {
            if let Some(image) = images.get_mut(&sink.image) {
                upload_frame_to_image(image, &frame);
            }
        }
    }
}

fn upload_frame_to_image(image: &mut Image, frame: &CameraFrame) {
    // Resize if the dimensions don't match the current image.
    let want = Extent3d {
        width: frame.width as u32,
        height: frame.height as u32,
        depth_or_array_layers: 1,
    };
    if image.texture_descriptor.size != want
        || image.texture_descriptor.format != TextureFormat::Rgba8Unorm
    {
        *image = Image::new_fill(
            want,
            TextureDimension::D2,
            &[0, 0, 0, 255],
            TextureFormat::Rgba8Unorm,
            RenderAssetUsages::default(),
        );
    }

    // Expand L8 → RGBA8 row-by-row (stride may be > width).
    let data = image.data.as_mut_slice();
    let stride = frame.stride as usize;
    let w = frame.width as usize;
    let h = frame.height as usize;
    for y in 0..h {
        let src_row = &frame.pixels[y * stride..y * stride + w];
        let dst_off = y * w * 4;
        for x in 0..w {
            let v = src_row[x];
            data[dst_off + x * 4] = v;
            data[dst_off + x * 4 + 1] = v;
            data[dst_off + x * 4 + 2] = v;
            data[dst_off + x * 4 + 3] = 255;
        }
    }
}
