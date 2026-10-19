//! Bevy plugin for the Tilt Five AR headset.
//!
//! The entry point is [`TiltFivePlugin`]. It bundles the following sub-plugins:
//! glasses lifecycle + rendering, wand input, headset camera streams, and
//! (behind the `mixcast` feature) a Mixcast-style spectator compositor.

pub mod bridge;
pub mod conversions;
pub mod plugin;
pub mod render;
pub mod wand;
pub mod camera_stream;

#[cfg(feature = "mixcast")]
pub mod mixcast;

pub use bridge::{
    BoardPose, BridgeError, CameraFrame, Glasses, PoseUsage, StagePose, T5GameboardType,
    WandButtons, WandHand, WandId, WandReport, WandStreamEvent,
    DEFAULT_GLASSES_FOV, DEFAULT_GLASSES_HEIGHT, DEFAULT_GLASSES_WIDTH,
};
pub use plugin::{
    AvailableGlasses, Board, BoardBundle, DebugGizmo, GlassesInfo, TiltFiveClientEvent,
    TiltFiveCommand, TiltFivePlugin,
};
pub use wand::{Wand, WandAnalog, WandButtonsState, WandOwner, WandPlugin};
pub use camera_stream::{CameraStreamPlugin, CameraStreamSink, HeadsetCameraFrame};

#[cfg(feature = "mixcast")]
pub use mixcast::{MixcastPlugin, MixcastSettings, SpectatorCamera, WebcamPlate};
