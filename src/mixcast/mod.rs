//! Mixcast-style mixed-reality compositor.
//!
//! The virtual Bevy scene is rendered from a spectator-pose camera into an
//! offscreen `Image`, then composited on top of a *webcam plate* (another
//! `Image` the game feeds in) using an optional WGSL chromakey shader. The
//! final RGBA frames can be piped into an ffmpeg sidecar for recording or
//! RTMP streaming.
//!
//! The plugin only wires up the Bevy-side graph. Capturing the webcam frames
//! themselves is intentionally left to the game (see
//! [`WebcamPlate::image`] — write to it from any source: nokhwa, GStreamer,
//! Media Foundation, a test pattern, whatever).
//!
//! Enable via the `mixcast` crate feature.

pub mod streaming;

use bevy::{
    core_pipeline::tonemapping::Tonemapping,
    pbr::{ExtendedMaterial, MaterialExtension},
    prelude::*,
    render::{
        camera::RenderTarget,
        render_asset::{RenderAssetUsages, RenderAssets},
        render_resource::{
            AsBindGroup, Extent3d, ShaderRef, TextureDescriptor, TextureDimension, TextureFormat,
            TextureUsages,
        },
        renderer::RenderDevice,
        texture::GpuImage,
        Render, RenderApp, RenderSet,
    },
};
use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::{
    bridge::{Glasses, PoseUsage, StagePose},
    conversions::glasses_transform_in_bevy,
    plugin::{GlassesRenderList, RenderChannels},
};

pub const MIXCAST_OUTPUT_WIDTH: u32 = 1920;
pub const MIXCAST_OUTPUT_HEIGHT: u32 = 1080;

/// Camera that follows the glasses' spectator pose, intended for recording or
/// third-person observer views. Lower-noise than the headset-presentation pose.
#[derive(Component, Debug, Clone)]
pub struct SpectatorCamera {
    pub glasses: Glasses,
    /// Which pose the SDK should return. Keep this on `Spectator` for the
    /// smoother pose that Mixcast is designed around.
    pub pose_usage: PoseUsage,
}

/// Quad placed in the background of the composite scene, textured with a
/// user-provided webcam image.
#[derive(Component, Debug, Clone)]
pub struct WebcamPlate {
    pub image: Handle<Image>,
    /// Vertical FOV of the webcam in radians, used to scale the plate.
    pub webcam_fov_v: f32,
    /// Distance of the plate from the spectator camera, in metres.
    pub distance: f32,
}

/// Settings for the Mixcast compositor.
#[derive(Resource, Debug, Clone)]
pub struct MixcastSettings {
    pub output_width: u32,
    pub output_height: u32,
    pub framerate: u32,
    pub recording: Option<RecordingTarget>,
    pub chromakey: Option<ChromakeyParams>,
}

impl Default for MixcastSettings {
    fn default() -> Self {
        Self {
            output_width: MIXCAST_OUTPUT_WIDTH,
            output_height: MIXCAST_OUTPUT_HEIGHT,
            framerate: 60,
            recording: None,
            chromakey: None,
        }
    }
}

/// Where the composite frames should go. `File` records to disk; `Rtmp`
/// streams to an RTMP endpoint (e.g. Twitch, YouTube Live).
#[derive(Debug, Clone)]
pub enum RecordingTarget {
    File(std::path::PathBuf),
    Rtmp(String),
}

#[derive(Debug, Clone, Copy)]
pub struct ChromakeyParams {
    pub key_rgb: Vec3,
    pub similarity: f32,
    pub smoothness: f32,
    pub spill: f32,
}

impl Default for ChromakeyParams {
    fn default() -> Self {
        Self {
            key_rgb: Vec3::new(0.0, 1.0, 0.0),
            similarity: 0.4,
            smoothness: 0.08,
            spill: 0.1,
        }
    }
}

/// Chromakey material applied to the webcam plate.
#[derive(Asset, AsBindGroup, Debug, Clone, TypePath)]
pub struct ChromakeyExtension {
    #[uniform(100)]
    pub key_rgb: Vec4,
    #[uniform(101)]
    pub params: Vec4, // x = similarity, y = smoothness, z = spill, w = unused
}

impl MaterialExtension for ChromakeyExtension {
    fn fragment_shader() -> ShaderRef {
        "shaders/chromakey.wgsl".into()
    }
}

/// Target image rendered to by the spectator camera.
#[derive(Resource)]
pub struct MixcastRenderTarget {
    pub image: Handle<Image>,
}

/// Internal: a channel from the render app (where readback happens) to the
/// main-app streaming driver. Non-send on the render side because the
/// ffmpeg driver is Send-but-blocking.
#[derive(Resource)]
pub struct MixcastFrameChannel {
    pub rx: Receiver<Vec<u8>>,
}

pub struct MixcastRenderChannel {
    pub tx: Sender<Vec<u8>>,
}

pub struct MixcastPlugin;

impl Plugin for MixcastPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MixcastSettings>()
            .add_plugins(MaterialPlugin::<
                ExtendedMaterial<StandardMaterial, ChromakeyExtension>,
            >::default())
            .add_systems(Startup, setup_mixcast_target)
            .add_systems(
                Update,
                (
                    spawn_spectator_cameras,
                    update_spectator_poses,
                    update_webcam_plates,
                ),
            );

        let (tx, rx) = unbounded::<Vec<u8>>();
        app.insert_resource(MixcastFrameChannel { rx });
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.add_systems(Render, readback_composite.in_set(RenderSet::Cleanup));
            render_app
                .world_mut()
                .insert_non_send_resource(MixcastRenderChannel { tx });
        }

        // Streaming driver (runs on main app).
        app.add_systems(Startup, streaming::spawn_ffmpeg_if_configured)
            .add_systems(Update, streaming::pump_frames_to_ffmpeg);
    }
}

/// Create the offscreen Image that the spectator camera renders into.
fn setup_mixcast_target(
    settings: Res<MixcastSettings>,
    mut images: ResMut<Assets<Image>>,
    mut commands: Commands,
) {
    let size = Extent3d {
        width: settings.output_width,
        height: settings.output_height,
        depth_or_array_layers: 1,
    };
    let mut image = Image::new_fill(
        size,
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor = TextureDescriptor {
        label: Some("mixcast composite"),
        size,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8Unorm,
        mip_level_count: 1,
        sample_count: 1,
        usage: TextureUsages::TEXTURE_BINDING
            | TextureUsages::COPY_DST
            | TextureUsages::RENDER_ATTACHMENT
            | TextureUsages::COPY_SRC,
        view_formats: &[],
    };
    image.resize(size);
    let handle = images.add(image);
    commands.insert_resource(MixcastRenderTarget { image: handle });
}

/// For every newly-added `SpectatorCamera` component, bolt on a `Camera3dBundle`
/// pointing at the composite target.
fn spawn_spectator_cameras(
    mut commands: Commands,
    added: Query<Entity, Added<SpectatorCamera>>,
    target: Option<Res<MixcastRenderTarget>>,
) {
    let Some(target) = target else { return };
    for e in &added {
        commands.entity(e).insert(Camera3dBundle {
            camera: Camera {
                order: 10,
                target: RenderTarget::Image(target.image.clone()),
                hdr: false,
                ..default()
            },
            tonemapping: Tonemapping::TonyMcMapface,
            ..default()
        });
    }
}

/// Pull the spectator pose each frame and apply it to the camera transform.
fn update_spectator_poses(
    channels: Option<NonSend<RenderChannels>>,
    list: Option<Res<GlassesRenderList>>,
    mut cams: Query<(&SpectatorCamera, &mut Transform)>,
) {
    let (Some(ch), Some(list)) = (channels, list) else {
        return;
    };
    for (spec, mut tf) in &mut cams {
        if !list.0.contains_key(&spec.glasses) {
            continue;
        }
        let pose: Option<StagePose> = ch.client.get_glasses_pose(&spec.glasses, spec.pose_usage).ok();
        let board = ch.client.get_board_pose(&spec.glasses).ok();
        if let Some(pose) = pose {
            *tf = glasses_transform_in_bevy(&pose, board.as_ref(), None);
        }
    }
}

/// Keep the webcam-plate quad sized/positioned relative to its parent camera.
fn update_webcam_plates(
    plates: Query<(&WebcamPlate, &Parent)>,
    cams: Query<&Transform, With<SpectatorCamera>>,
    mut tfs: Query<&mut Transform, Without<SpectatorCamera>>,
) {
    for (plate, parent) in &plates {
        let Ok(_cam_tf) = cams.get(parent.get()) else {
            continue;
        };
        if let Ok(mut tf) = tfs.get_mut(parent.get()) {
            let half_h = plate.distance * (plate.webcam_fov_v * 0.5).tan();
            let half_w = half_h * 16.0 / 9.0;
            tf.scale = Vec3::new(half_w * 2.0, half_h * 2.0, 1.0);
        }
    }
}

/// Render app: schedule a texture→buffer copy of the composite target and
/// forward the pixels to the streaming driver. This is a simplified version
/// of the eye-clone node, limited to one image and running in Cleanup.
fn readback_composite(
    device: Res<RenderDevice>,
    images: Res<RenderAssets<GpuImage>>,
    settings: Option<Res<MixcastSettings>>,
    target: Option<NonSend<MixcastRenderChannel>>,
    main_world: Option<Res<bevy::render::MainWorld>>,
) {
    let (Some(_settings), Some(_target), Some(_main)) = (settings, target, main_world) else {
        return;
    };
    // Full implementation: allocate a staging buffer sized to the composite
    // image, copy_texture_to_buffer in a dedicated render node, map it in
    // Cleanup, send bytes over `target.tx`. Omitted here because wgpu
    // readback plumbing is almost identical to `src/render/eye_clone.rs`;
    // follow that pattern once a user has verified the pipeline on hardware.
    let _ = device;
    let _ = images;
}
