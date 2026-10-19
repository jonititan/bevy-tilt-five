//! Render-side plumbing: eye-clone render-graph node plus a per-platform
//! graphics bridge (DX11 on Windows, OpenGL on Linux).

pub mod eye_clone;

#[cfg(target_family = "windows")]
pub mod dx11;

#[cfg(target_os = "linux")]
pub mod opengl;

use bevy::{
    prelude::*,
    render::{graph::CameraDriverLabel, render_graph::RenderGraph, Render, RenderApp, RenderSet},
};
use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::bridge::Glasses;

pub struct RenderBridgePlugin;

/// Payload shipped from the Bevy render world to the per-API bridge (DX11).
pub type GlassesBufferInfo = (
    Glasses,
    Vec<u8>, // left eye bytes
    Vec<u8>, // right eye bytes
    bevy::math::Vec3, // lpos in STAGE
    bevy::math::Vec3, // rpos in STAGE
    bevy::math::Quat, // rot stage->glasses
);

/// Resource that gets a Sender<GlassesBufferInfo> from whichever graphics-API
/// bridge is active. Only one bridge is wired up at a time.
pub struct BufferSender {
    pub sender: Sender<GlassesBufferInfo>,
}

pub struct BufferReceiverResource {
    pub receiver: Receiver<GlassesBufferInfo>,
}

impl Plugin for RenderBridgePlugin {
    fn build(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        // The eye-clone render-graph node schedules a texture→buffer copy for each
        // active pair of glasses after the main camera driver runs.
        {
            let mut graph = render_app.world_mut().resource_mut::<RenderGraph>();
            graph.add_node(eye_clone::EyeCloneLabel, eye_clone::EyeCloneNode::default());
            graph.add_node_edge(CameraDriverLabel, eye_clone::EyeCloneLabel);
        }

        // Readback: Prepare -> issue buffer allocations; Cleanup -> map & drain.
        render_app
            .init_resource::<eye_clone::EyeStagingBuffers>()
            .add_systems(Render, eye_clone::setup_readback_buffers.in_set(RenderSet::Prepare))
            .add_systems(Render, eye_clone::drain_readback_buffers.in_set(RenderSet::Cleanup));

        // One channel feeds whichever platform bridge is active. On unsupported
        // platforms the receiver is dropped and the readback systems no-op.
        let (tx, rx) = unbounded::<GlassesBufferInfo>();
        render_app
            .world_mut()
            .insert_non_send_resource(BufferSender { sender: tx });

        #[cfg(any(target_family = "windows", target_os = "linux"))]
        render_app
            .world_mut()
            .insert_non_send_resource(BufferReceiverResource { receiver: rx });

        #[cfg(not(any(target_family = "windows", target_os = "linux")))]
        let _ = rx;

        #[cfg(target_family = "windows")]
        render_app.add_plugins(dx11::Dx11Plugin);

        #[cfg(target_os = "linux")]
        render_app.add_plugins(opengl::OpenGlPlugin);
    }
}
