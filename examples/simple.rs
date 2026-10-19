use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiPlugin};
use bevy_tilt_five::{
    AvailableGlasses, BoardBundle, CameraStreamPlugin, GlassesInfo, TiltFiveClientEvent,
    TiltFiveCommand, TiltFivePlugin, WandPlugin,
};

fn main() {
    App::new()
        .insert_resource(ClearColor(Color::BLACK))
        .add_plugins(DefaultPlugins)
        .add_plugins((EguiPlugin, TiltFivePlugin, WandPlugin, CameraStreamPlugin))
        .add_systems(Startup, setup)
        .add_systems(Update, ui)
        .run();
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // Floor
    commands.spawn(PbrBundle {
        mesh: meshes.add(Plane3d::default().mesh().size(5.0, 5.0)),
        material: materials.add(Color::srgb(0.6, 0.9, 0.5)),
        ..default()
    });

    // Light
    commands.spawn(PointLightBundle {
        point_light: PointLight {
            intensity: 1_000_000.0,
            shadows_enabled: true,
            ..default()
        },
        transform: Transform::from_xyz(4.0, 8.0, 4.0),
        ..default()
    });

    // Observer camera (for the main monitor window)
    commands.spawn(Camera3dBundle {
        transform: Transform::from_xyz(-2.0, 2.5, 4.0).looking_at(Vec3::ZERO, Vec3::Y),
        ..default()
    });

    // Tilt Five gameboard. Setting the Transform here rotates the whole
    // Tilt Five scene — e.g. `with_rotation(Quat::from_rotation_y(0.5))` to
    // yaw the board 29°.
    commands.spawn(BoardBundle::default());
}

fn ui(
    mut egui_context: EguiContexts,
    mut events: EventWriter<TiltFiveCommand>,
    mut read_events: EventReader<TiltFiveClientEvent>,
    glasses: Res<AvailableGlasses>,
) {
    let ctx = egui_context.ctx_mut();
    egui::Window::new("T5 Status").show(ctx, |ui| {
        ui.label("Available Glasses:");
        for (key, val) in glasses.glasses.iter() {
            let label: &str = key.into();
            if val.is_disconnected() && ui.button(label).clicked() {
                events.send(TiltFiveCommand::ConnectToGlasses(key.clone()));
            }
        }

        ui.separator();
        ui.label("Connected:");
        for (key, info) in glasses.glasses.iter() {
            if let GlassesInfo::Connected { friendly_name, .. } = info {
                let name = friendly_name.clone().unwrap_or_else(|| key.to_string());
                ui.label(name);
                if ui.button("Disconnect").clicked() {
                    events.send(TiltFiveCommand::DisconnectFromGlasses(key.clone()));
                }
            }
        }

        ui.separator();
        if ui.button("Refresh List").clicked() {
            events.send(TiltFiveCommand::RefreshGlassesList);
        }

        for evt in read_events.read() {
            if let TiltFiveClientEvent::Error(msg) = evt {
                ui.colored_label(egui::Color32::RED, msg);
            }
        }
    });
}
