//! Core plugin: glasses lifecycle, board entity, two-eye camera rig.
//!
//! The plugin spawns a `T5Client` inside the Bevy *render* sub-app (so that
//! the SDK can share the GPU graphics context when we later initialise the
//! DX11 bridge). Commands cross from main → render via a crossbeam channel;
//! events flow back the other way.

use std::f32::consts::PI;

use bevy::{
    core_pipeline::tonemapping::{DebandDither, Tonemapping},
    prelude::*,
    render::{
        camera::RenderTarget,
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        render_asset::RenderAssetUsages,
        render_resource::{
            Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
        },
        Render, RenderApp, RenderSet,
    },
    utils::HashMap,
};
use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::{
    bridge::{
        BoardPose, Glasses, PoseUsage, StagePose, T5Client, DEFAULT_GLASSES_FOV,
        DEFAULT_GLASSES_HEIGHT, DEFAULT_GLASSES_WIDTH,
    },
    conversions::glasses_transform_in_bevy,
};

/// Public events emitted by the plugin to the game.
#[derive(Debug, Clone, Event)]
pub enum TiltFiveClientEvent {
    GlassesFound(Vec<Glasses>),
    GlassesConnected(Glasses, Option<String>),
    GlassesDisconnected(Glasses),
    GlassesPoseChanged {
        glasses: Glasses,
        transform: Transform,
        ipd_meters: f32,
    },
    Error(String),
}

/// Commands the game sends to the plugin.
#[derive(Debug, Clone, Event)]
pub enum TiltFiveCommand {
    RefreshGlassesList,
    ConnectToGlasses(Glasses),
    DisconnectFromGlasses(Glasses),
    /// Replace the images used for the eye render targets (used internally
    /// once the per-glasses cameras are spawned).
    SetGlassesImages(Glasses, Handle<Image>, Handle<Image>),
}

/// Marker for the gameboard anchor entity. Its `Transform` is the authoritative
/// Bevy-space pose of the physical board — set it to tilt the board, mount it
/// to a moving rig, scale the world, etc.
#[derive(Component, Reflect, Debug, Default)]
pub struct Board;

#[derive(Bundle, Default)]
pub struct BoardBundle {
    pub board: Board,
    pub spatial: SpatialBundle,
}

/// Resource listing all glasses the service has reported, with per-glasses
/// state (connected or not). Reflected so the inspector sees it.
#[derive(Resource, Reflect, Debug, Default, Clone, ExtractResource)]
#[reflect(Resource)]
pub struct AvailableGlasses {
    pub glasses: HashMap<Glasses, GlassesInfo>,
}

#[derive(Reflect, Debug, Default, Clone)]
pub enum GlassesInfo {
    #[default]
    Disconnected,
    Connected {
        friendly_name: Option<String>,
        entity: Entity,
        left: Handle<Image>,
        right: Handle<Image>,
    },
}

impl GlassesInfo {
    pub fn is_disconnected(&self) -> bool {
        matches!(self, GlassesInfo::Disconnected)
    }
}

/// Component on a glasses-root entity (parented under the Board). Holds the
/// three cached handles for GL<-->bevy wiring.
#[derive(Component)]
pub struct TiltFiveGlasses {
    pub id: Glasses,
    pub left: Handle<Image>,
    pub right: Handle<Image>,
}

#[derive(Component)]
pub struct TiltFiveIpd(pub f32);

/// Visual debug gizmo: an axis helper plus a coloured sphere.
#[derive(Component)]
pub struct DebugGizmo(pub Color);

/// Channel endpoints owned by the main-world side of the plugin.
#[derive(Resource)]
pub(crate) struct MainChannels {
    pub to_render: Sender<TiltFiveCommand>,
    pub from_render: Receiver<TiltFiveClientEvent>,
}

/// Non-send resource on the render app that owns the T5 client.
pub(crate) struct RenderChannels {
    pub client: T5Client,
    pub to_main: Sender<TiltFiveClientEvent>,
    pub from_main: Receiver<TiltFiveCommand>,
}

/// Per-glasses render-side state.
#[derive(Default)]
pub struct GlassesRenderEntry {
    pub left: Option<Handle<Image>>,
    pub right: Option<Handle<Image>>,
    pub last_pose: Option<(Vec3, Vec3, Quat)>, // (lpos, rpos, rot) in STAGE
    pub last_board: Option<BoardPose>,
}

#[derive(Resource, Default)]
pub struct GlassesRenderList(pub HashMap<Glasses, GlassesRenderEntry>);

pub const GLASSES_TEXTURE_SIZE: Extent3d = Extent3d {
    width: DEFAULT_GLASSES_WIDTH,
    height: DEFAULT_GLASSES_HEIGHT,
    depth_or_array_layers: 1,
};

pub const TEXTURE_FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;

pub struct TiltFivePlugin;

impl Plugin for TiltFivePlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<TiltFiveClientEvent>()
            .add_event::<TiltFiveCommand>()
            .init_resource::<AvailableGlasses>()
            .register_type::<AvailableGlasses>()
            .register_type::<Board>()
            .add_plugins(ExtractResourcePlugin::<AvailableGlasses>::default());

        let client = match T5Client::new("bevy-tilt-five", env!("CARGO_PKG_VERSION")) {
            Ok(c) => c,
            Err(e) => {
                error!("could not initialise Tilt Five client: {e}");
                return;
            }
        };

        let (to_render, from_main) = unbounded::<TiltFiveCommand>();
        let (to_main, from_render) = unbounded::<TiltFiveClientEvent>();

        app.insert_resource(MainChannels { to_render, from_render });

        app.add_systems(
            Update,
            (
                setup_board_transformer,
                setup_debug_gizmo,
                pump_channels,
                update_glasses_list,
                connect_to_glasses,
                disconnect_from_glasses,
                setup_glasses_rendering,
                set_glasses_position,
                adjust_glasses_position,
            ),
        );

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_resource::<GlassesRenderList>()
            .add_systems(ExtractSchedule, process_render_commands)
            .add_systems(Render, (pull_poses,).in_set(RenderSet::ExtractCommands));
        render_app
            .world_mut()
            .insert_non_send_resource(RenderChannels { client, to_main, from_main });

        // Rendering sub-plugin (texture copy node + DX11 bridge)
        app.add_plugins(crate::render::RenderBridgePlugin);
    }
}

/// Spawn an invisible transformer child under each Board so later entity
/// parenting uses a fresh frame when the user wants to introduce an extra
/// rotation (e.g. inverting Z for a reversed table orientation).
fn setup_board_transformer(mut commands: Commands, boards: Query<Entity, Added<Board>>) {
    for entity in &boards {
        commands.entity(entity).with_children(|p| {
            p.spawn((BoardTransformer, SpatialBundle::default()));
        });
    }
}

#[derive(Component)]
pub(crate) struct BoardTransformer;

fn setup_debug_gizmo(
    mut commands: Commands,
    gizmos: Query<(Entity, &DebugGizmo), Added<DebugGizmo>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if gizmos.is_empty() {
        return;
    }

    let axis_mesh = meshes.add(Cuboid::new(0.05, 0.05, 2.0));
    let ball_mesh = meshes.add(Sphere::new(0.1).mesh().ico(6).unwrap());
    let box_mesh = meshes.add(Cuboid::new(0.1, 0.1, 0.1));
    let axis_material = materials.add(Color::srgb(0.7, 0.7, 0.7));
    let x_mat = materials.add(Color::srgb(1.0, 0.0, 0.0));
    let y_mat = materials.add(Color::srgb(0.0, 1.0, 0.0));
    let z_mat = materials.add(Color::srgb(0.0, 0.0, 1.0));

    let axis = [(Vec3::X, x_mat), (Vec3::Y, y_mat), (Vec3::Z, z_mat)];

    for (entity, gizmo) in &gizmos {
        let my_mat = materials.add(gizmo.0);
        commands.entity(entity).with_children(|p| {
            p.spawn(PbrBundle {
                mesh: ball_mesh.clone(),
                material: my_mat,
                ..default()
            });
            for (ax, color) in &axis {
                p.spawn(PbrBundle {
                    mesh: axis_mesh.clone(),
                    material: axis_material.clone(),
                    transform: Transform::IDENTITY.looking_at(-*ax, ax.any_orthogonal_vector()),
                    ..default()
                });
                p.spawn(PbrBundle {
                    mesh: ball_mesh.clone(),
                    material: color.clone(),
                    transform: Transform::from_translation(*ax),
                    ..default()
                });
                p.spawn(PbrBundle {
                    mesh: box_mesh.clone(),
                    material: color.clone(),
                    transform: Transform::from_translation(-*ax),
                    ..default()
                });
            }
        });
    }
}

/// Shuttle commands/events across main↔render channels so client code can
/// use normal Bevy `EventReader`/`EventWriter` on both sides.
fn pump_channels(
    channels: Res<MainChannels>,
    mut commands_in: EventReader<TiltFiveCommand>,
    mut events_out: EventWriter<TiltFiveClientEvent>,
) {
    for cmd in commands_in.read() {
        let _ = channels.to_render.send(cmd.clone());
    }
    while let Ok(evt) = channels.from_render.try_recv() {
        events_out.send(evt);
    }
}

fn update_glasses_list(
    mut list: ResMut<AvailableGlasses>,
    mut reader: EventReader<TiltFiveClientEvent>,
) {
    for evt in reader.read() {
        if let TiltFiveClientEvent::GlassesFound(new_list) = evt {
            for g in new_list {
                list.glasses.entry(g.clone()).or_insert(GlassesInfo::default());
            }
            break;
        }
    }
}

fn connect_to_glasses(
    mut list: ResMut<AvailableGlasses>,
    mut events: EventReader<TiltFiveClientEvent>,
    mut cmds: Commands,
    mut images: ResMut<Assets<Image>>,
) {
    for evt in events.read() {
        if let TiltFiveClientEvent::GlassesConnected(gid, friendly_name) = evt {
            if !matches!(list.glasses.get(gid), Some(GlassesInfo::Disconnected) | None) {
                continue;
            }

            let left = images.add(blank_eye_image());
            let right = images.add(blank_eye_image());

            let entity = cmds
                .spawn((
                    SpatialBundle::default(),
                    TiltFiveGlasses {
                        id: gid.clone(),
                        left: left.clone(),
                        right: right.clone(),
                    },
                ))
                .id();

            list.glasses.insert(
                gid.clone(),
                GlassesInfo::Connected {
                    entity,
                    left,
                    right,
                    friendly_name: friendly_name.clone(),
                },
            );
        }
    }
}

fn blank_eye_image() -> Image {
    let mut img = Image::new_fill(
        GLASSES_TEXTURE_SIZE,
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TEXTURE_FORMAT,
        RenderAssetUsages::default(),
    );
    img.texture_descriptor = TextureDescriptor {
        label: None,
        size: GLASSES_TEXTURE_SIZE,
        dimension: TextureDimension::D2,
        format: TEXTURE_FORMAT,
        mip_level_count: 1,
        sample_count: 1,
        usage: TextureUsages::TEXTURE_BINDING
            | TextureUsages::COPY_DST
            | TextureUsages::RENDER_ATTACHMENT
            | TextureUsages::COPY_SRC,
        view_formats: &[],
    };
    img.resize(GLASSES_TEXTURE_SIZE);
    img
}

fn disconnect_from_glasses(
    mut list: ResMut<AvailableGlasses>,
    mut events: EventReader<TiltFiveClientEvent>,
    mut cmds: Commands,
) {
    for evt in events.read() {
        if let TiltFiveClientEvent::GlassesDisconnected(gid) = evt {
            if let Some(GlassesInfo::Connected { entity, .. }) = list.glasses.get(gid) {
                cmds.entity(*entity).despawn_recursive();
            }
            list.glasses.insert(gid.clone(), GlassesInfo::Disconnected);
        }
    }
}

fn setup_glasses_rendering(
    mut commands: Commands,
    query: Query<(Entity, &TiltFiveGlasses), Added<TiltFiveGlasses>>,
    boards: Query<Entity, With<BoardTransformer>>,
    mut t5_commands: EventWriter<TiltFiveCommand>,
) {
    let Ok(board) = boards.get_single() else {
        return;
    };
    for (entity, glasses) in &query {
        let TiltFiveGlasses { id, left, right } = glasses;
        commands.entity(entity).with_children(|parent| {
            parent.spawn((
                Camera3dBundle {
                    transform: Transform::from_xyz(-0.03, 0.0, 0.0)
                        .with_rotation(Quat::from_euler(EulerRot::XYZ, PI, 0.0, 0.0)),
                    camera: Camera {
                        order: -2,
                        target: RenderTarget::Image(left.clone()),
                        hdr: false,
                        ..default()
                    },
                    tonemapping: Tonemapping::TonyMcMapface,
                    projection: Projection::Perspective(PerspectiveProjection {
                        fov: DEFAULT_GLASSES_FOV.to_radians(),
                        aspect_ratio: DEFAULT_GLASSES_WIDTH as f32
                            / DEFAULT_GLASSES_HEIGHT as f32,
                        ..default()
                    }),
                    ..default()
                },
                DebandDither::Enabled,
            ));
            parent.spawn((
                Camera3dBundle {
                    transform: Transform::from_xyz(0.03, 0.0, 0.0)
                        .with_rotation(Quat::from_euler(EulerRot::XYZ, PI, 0.0, 0.0)),
                    camera: Camera {
                        order: -1,
                        target: RenderTarget::Image(right.clone()),
                        hdr: false,
                        ..default()
                    },
                    tonemapping: Tonemapping::TonyMcMapface,
                    projection: Projection::Perspective(PerspectiveProjection {
                        fov: DEFAULT_GLASSES_FOV.to_radians(),
                        aspect_ratio: DEFAULT_GLASSES_WIDTH as f32
                            / DEFAULT_GLASSES_HEIGHT as f32,
                        ..default()
                    }),
                    ..default()
                },
                DebandDither::Enabled,
            ));
        });
        t5_commands.send(TiltFiveCommand::SetGlassesImages(
            id.clone(),
            left.clone(),
            right.clone(),
        ));
        commands.entity(board).add_child(entity);
    }
}

fn adjust_glasses_position(
    mut cameras: Query<(&Camera3d, &mut Transform)>,
    parents: Query<(&TiltFiveIpd, &Children), Changed<TiltFiveIpd>>,
) {
    for (ipd, children) in &parents {
        let half = ipd.0 / 2.0;
        for child in children.iter() {
            if let Ok((_, mut t)) = cameras.get_mut(*child) {
                t.translation = t.translation.normalize_or_zero() * half;
            }
        }
    }
}

fn set_glasses_position(
    mut commands: Commands,
    list: Res<AvailableGlasses>,
    mut events: EventReader<TiltFiveClientEvent>,
) {
    for evt in events.read() {
        if let TiltFiveClientEvent::GlassesPoseChanged {
            glasses,
            transform,
            ipd_meters,
        } = evt
        {
            if let Some(GlassesInfo::Connected { entity, .. }) = list.glasses.get(glasses) {
                commands.entity(*entity).insert((*transform, TiltFiveIpd(*ipd_meters)));
            }
        }
    }
}

// --- Render-app systems ---

/// Executes command messages that cross from main → render. Owns the T5 client.
fn process_render_commands(
    mut ch: NonSendMut<RenderChannels>,
    mut list: ResMut<GlassesRenderList>,
) {
    while let Ok(cmd) = ch.from_main.try_recv() {
        match cmd {
            TiltFiveCommand::RefreshGlassesList => match ch.client.list_glasses() {
                Ok(new_list) => {
                    let _ = ch.to_main.send(TiltFiveClientEvent::GlassesFound(new_list));
                }
                Err(e) => {
                    let _ = ch.to_main.send(TiltFiveClientEvent::Error(e.to_string()));
                }
            },
            TiltFiveCommand::ConnectToGlasses(gid) => {
                if list.0.contains_key(&gid) {
                    continue;
                }
                match ch.client.create_glasses(&gid) {
                    Ok((_, friendly)) => {
                        let _ = ch.client.configure_wand_stream(&gid, true);
                        list.0.insert(gid.clone(), GlassesRenderEntry::default());
                        let _ = ch
                            .to_main
                            .send(TiltFiveClientEvent::GlassesConnected(gid, friendly));
                    }
                    Err(e) => {
                        let _ = ch.to_main.send(TiltFiveClientEvent::Error(e.to_string()));
                    }
                }
            }
            TiltFiveCommand::DisconnectFromGlasses(gid) => {
                let _ = ch.client.release_glasses(&gid);
                list.0.remove(&gid);
                let _ = ch
                    .to_main
                    .send(TiltFiveClientEvent::GlassesDisconnected(gid));
            }
            TiltFiveCommand::SetGlassesImages(gid, left, right) => {
                if let Some(entry) = list.0.get_mut(&gid) {
                    entry.left = Some(left);
                    entry.right = Some(right);
                }
            }
        }
    }
}

/// Pull per-frame pose and (if available) board-pose from the SDK, convert
/// to Bevy space, and publish as events.
fn pull_poses(ch: NonSendMut<RenderChannels>, mut list: ResMut<GlassesRenderList>) {
    let ids: Vec<Glasses> = list.0.keys().cloned().collect();
    for id in ids {
        let pose: Option<StagePose> = ch.client.get_glasses_pose(&id, PoseUsage::Glasses).ok();
        let ipd = ch.client.get_ipd(&id).ok();
        let board = ch.client.get_board_pose(&id).ok();

        if let (Some(pose), Some(ipd_mm)) = (pose, ipd) {
            let transform = glasses_transform_in_bevy(&pose, board.as_ref(), None);
            let ipd_m = ipd_mm * 0.001;
            if let Some(entry) = list.0.get_mut(&id) {
                let lpos = transform.translation + transform.left() * (ipd_m / 2.0);
                let rpos = transform.translation + transform.right() * (ipd_m / 2.0);
                entry.last_pose = Some((lpos, rpos, pose.rot_stage_to_glasses));
                entry.last_board = board;
            }
            let _ = ch.to_main.send(TiltFiveClientEvent::GlassesPoseChanged {
                glasses: id,
                transform,
                ipd_meters: ipd_m,
            });
        }
    }
}
