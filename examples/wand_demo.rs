use bevy::prelude::*;
use bevy_tilt_five::{
    BoardBundle, TiltFivePlugin, Wand, WandAnalog, WandButtonsState, WandPlugin,
};

fn main() {
    App::new()
        .insert_resource(ClearColor(Color::BLACK))
        .add_plugins(DefaultPlugins)
        .add_plugins((TiltFivePlugin, WandPlugin))
        .add_systems(Startup, setup)
        .add_systems(Update, (render_wand_rays, log_wand_buttons))
        .run();
}

fn setup(mut commands: Commands) {
    commands.spawn(BoardBundle::default());
    commands.spawn(Camera3dBundle {
        transform: Transform::from_xyz(-2.0, 2.5, 4.0).looking_at(Vec3::ZERO, Vec3::Y),
        ..default()
    });
    commands.spawn(DirectionalLightBundle {
        transform: Transform::from_xyz(1.0, 5.0, 0.0).looking_at(Vec3::ZERO, Vec3::Y),
        ..default()
    });
}

fn render_wand_rays(mut gizmos: Gizmos, wands: Query<(&Transform, &Wand)>) {
    for (t, _wand) in &wands {
        // Draw a short ray out of the grip in the wand's forward direction.
        let origin = t.translation;
        let forward = t.rotation * -Vec3::Z;
        gizmos.ray(origin, forward * 0.3, Color::srgb(1.0, 0.4, 0.8));
    }
}

fn log_wand_buttons(
    wands: Query<(&Wand, &WandButtonsState, &WandAnalog), Changed<WandButtonsState>>,
) {
    for (wand, buttons, analog) in &wands {
        info!(
            "wand {:?} buttons={buttons:?} trigger={:.2} stick={:?}",
            wand.id.0, analog.trigger, analog.stick
        );
    }
}
