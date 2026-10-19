use std::path::PathBuf;

use bevy::prelude::*;
use bevy_tilt_five::{
    bridge::Glasses, mixcast::ChromakeyParams, BoardBundle, MixcastPlugin, MixcastSettings,
    PoseUsage, SpectatorCamera, TiltFivePlugin, WebcamPlate,
};

fn main() {
    App::new()
        .insert_resource(ClearColor(Color::BLACK))
        .insert_resource(MixcastSettings {
            output_width: 1920,
            output_height: 1080,
            framerate: 60,
            recording: Some(bevy_tilt_five::mixcast::RecordingTarget::File(
                PathBuf::from("mixcast.mp4"),
            )),
            chromakey: Some(ChromakeyParams::default()),
        })
        .add_plugins(DefaultPlugins)
        .add_plugins((TiltFivePlugin, MixcastPlugin))
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn(BoardBundle::default());

    // Observer camera for the main window.
    commands.spawn(Camera3dBundle {
        transform: Transform::from_xyz(0.0, 2.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
        ..default()
    });

    // Spectator camera: drives the Mixcast composite. Point at your actual
    // glasses id — the plugin only pulls poses once that pair is connected.
    commands.spawn((
        SpectatorCamera {
            glasses: Glasses::from("your-glasses-id"),
            pose_usage: PoseUsage::Spectator,
        },
        WebcamPlate {
            // Load a placeholder image here, or feed in a nokhwa/gstreamer
            // webcam capture from your own system.
            image: asset_server.load("textures/webcam_placeholder.png"),
            webcam_fov_v: 50f32.to_radians(),
            distance: 2.0,
        },
    ));
}
