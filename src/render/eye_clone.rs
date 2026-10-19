//! Render-graph node that copies the two eye render-target textures into
//! staging buffers so the DX11 bridge can read the pixels back.

use std::sync::mpsc;

use bevy::{
    prelude::*,
    render::{
        render_asset::RenderAssets,
        render_graph::{Node, NodeRunError, RenderGraphContext, RenderLabel},
        render_resource::{
            Buffer, BufferDescriptor, BufferUsages, ImageCopyBuffer, ImageDataLayout, MapMode,
        },
        renderer::{RenderContext, RenderDevice},
        texture::GpuImage,
    },
    utils::HashMap,
};

use crate::bridge::{DEFAULT_GLASSES_HEIGHT, DEFAULT_GLASSES_WIDTH};
use crate::plugin::{GlassesRenderList, GLASSES_TEXTURE_SIZE, TEXTURE_FORMAT};
use crate::render::BufferSender;

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub struct EyeCloneLabel;

#[derive(Default)]
pub struct EyeCloneNode;

/// Resource holding per-glasses staging buffers used by the render node and
/// the Cleanup readback step.
#[derive(Resource, Default)]
pub struct EyeStagingBuffers {
    pub buffers: HashMap<crate::bridge::Glasses, (Buffer, Buffer)>,
}

/// Bytes per pixel for the eye texture format. Chosen format is RGBA8 Unorm.
fn bytes_per_row() -> u32 {
    DEFAULT_GLASSES_WIDTH * 4
}

fn padded_bytes_total() -> u64 {
    bytes_per_row() as u64 * (DEFAULT_GLASSES_HEIGHT as u64)
}

/// Prepare-set system: ensure every connected pair of glasses has an
/// up-to-date pair of staging buffers.
pub fn setup_readback_buffers(
    mut staging: ResMut<EyeStagingBuffers>,
    device: Res<RenderDevice>,
    list: Res<GlassesRenderList>,
) {
    for (gid, entry) in list.0.iter() {
        if entry.left.is_none() || entry.right.is_none() {
            continue;
        }
        if staging.buffers.contains_key(gid) {
            continue;
        }
        let left = device.create_buffer(&BufferDescriptor {
            label: Some("tilt5 eye readback (L)"),
            size: padded_bytes_total(),
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let right = device.create_buffer(&BufferDescriptor {
            label: Some("tilt5 eye readback (R)"),
            size: padded_bytes_total(),
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        staging.buffers.insert(gid.clone(), (left, right));
    }
    // drop buffers for glasses that disappeared
    let live: std::collections::HashSet<_> = list.0.keys().cloned().collect();
    staging.buffers.retain(|k, _| live.contains(k));
}

impl Node for EyeCloneNode {
    fn run<'w>(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        let list = world.resource::<GlassesRenderList>();
        let Some(staging) = world.get_resource::<EyeStagingBuffers>() else {
            return Ok(());
        };
        let images = world.resource::<RenderAssets<GpuImage>>();

        let bpr = bytes_per_row();

        for (gid, entry) in list.0.iter() {
            let (Some(left_h), Some(right_h)) = (entry.left.as_ref(), entry.right.as_ref()) else {
                continue;
            };
            let Some((lb, rb)) = staging.buffers.get(gid) else {
                continue;
            };

            if let Some(image) = images.get(left_h) {
                render_context.command_encoder().copy_texture_to_buffer(
                    image.texture.as_image_copy(),
                    ImageCopyBuffer {
                        buffer: lb,
                        layout: ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(bpr),
                            rows_per_image: Some(DEFAULT_GLASSES_HEIGHT),
                        },
                    },
                    GLASSES_TEXTURE_SIZE,
                );
            }
            if let Some(image) = images.get(right_h) {
                render_context.command_encoder().copy_texture_to_buffer(
                    image.texture.as_image_copy(),
                    ImageCopyBuffer {
                        buffer: rb,
                        layout: ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(bpr),
                            rows_per_image: Some(DEFAULT_GLASSES_HEIGHT),
                        },
                    },
                    GLASSES_TEXTURE_SIZE,
                );
            }

            let _ = (bpr, TEXTURE_FORMAT); // keep used
        }
        Ok(())
    }
}

/// Cleanup-set system: map each pair of staging buffers, read pixels back,
/// and ship (pixels, pose) to the graphics-API bridge via the Sender.
pub fn drain_readback_buffers(
    device: Res<RenderDevice>,
    staging: Res<EyeStagingBuffers>,
    list: Res<GlassesRenderList>,
    sender: NonSend<BufferSender>,
) {
    for (gid, (lb, rb)) in staging.buffers.iter() {
        let entry = match list.0.get(gid) {
            Some(e) => e,
            None => continue,
        };
        let Some((lpos, rpos, rot)) = entry.last_pose else {
            continue;
        };

        let ls = lb.slice(..);
        let rs = rb.slice(..);
        let (tx, rx) = mpsc::channel();
        let l_tx = tx.clone();
        let r_tx = tx;
        ls.map_async(MapMode::Read, move |_| {
            let _ = l_tx.send(());
        });
        rs.map_async(MapMode::Read, move |_| {
            let _ = r_tx.send(());
        });
        device.poll(bevy::render::render_resource::Maintain::Wait);

        if rx.recv_timeout(std::time::Duration::from_millis(16)).is_err() {
            continue;
        }
        if rx.recv_timeout(std::time::Duration::from_millis(16)).is_err() {
            continue;
        }

        let left_bytes = ls.get_mapped_range().to_vec();
        let right_bytes = rs.get_mapped_range().to_vec();
        lb.unmap();
        rb.unmap();

        let _ = sender
            .sender
            .send((gid.clone(), left_bytes, right_bytes, lpos, rpos, rot));
    }
}
