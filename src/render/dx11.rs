//! Windows DX11 bridge: creates a parallel D3D11 device, uploads the
//! wgpu-rendered eye textures into D3D11 textures, and hands them to the
//! Tilt Five SDK's `t5SendFrameToGlasses`.
//!
//! Note: wgpu uses D3D12 by default on Windows, so we maintain a *separate*
//! D3D11 device alongside the main renderer. The round trip is GPU → readback
//! buffer → CPU → D3D11 upload. Slow but correct.

use std::{ffi::c_void, ptr::null};

use anyhow::{bail, Result};
use bevy::{prelude::*, render::Render};

use windows::{
    core::Interface,
    Win32::Graphics::{
        Direct3D::D3D_DRIVER_TYPE_HARDWARE,
        Direct3D11::{
            D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
            D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_SINGLETHREADED, D3D11_SDK_VERSION,
            D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
        },
        Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_TYPELESS, DXGI_SAMPLE_DESC},
    },
};

use crate::bridge::{
    ffi::T5_FrameInfo__bindgen_ty_1, DEFAULT_GLASSES_FOV, DEFAULT_GLASSES_HEIGHT,
    DEFAULT_GLASSES_WIDTH,
};
use crate::plugin::RenderChannels;
use crate::render::{BufferReceiverResource, GlassesBufferInfo};

pub struct Dx11Plugin;

impl Plugin for Dx11Plugin {
    fn build(&self, app: &mut App) {
        app.insert_non_send_resource(Dx11State { device: None })
            .add_systems(Render, (setup_dx11, send_frames));
    }
}

struct Dx11State {
    device: Option<ID3D11Device>,
}

fn create_dx11_device() -> Result<ID3D11Device> {
    unsafe {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let hr = D3D11CreateDevice(
            None, // default adapter
            D3D_DRIVER_TYPE_HARDWARE,
            None, // no software module
            D3D11_CREATE_DEVICE_SINGLETHREADED,
            None,  // default feature levels
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        );
        if hr.is_err() {
            bail!("D3D11CreateDevice failed: {hr:?}");
        }
        device.ok_or_else(|| anyhow::anyhow!("D3D11CreateDevice returned null device"))
    }
}

fn setup_dx11(mut state: NonSendMut<Dx11State>, mut channels: NonSendMut<RenderChannels>) {
    if state.device.is_some() {
        return;
    }
    match create_dx11_device() {
        Ok(dev) => {
            let raw: *mut c_void = dev.as_raw() as *mut c_void;
            channels.client.set_dx11_graphics_context(raw);
            state.device = Some(dev);
        }
        Err(e) => error!("couldn't initialise D3D11 device: {e}"),
    }
}

fn send_frames(
    state: NonSend<Dx11State>,
    channels: NonSendMut<RenderChannels>,
    receiver: NonSend<BufferReceiverResource>,
) {
    let Some(device) = state.device.as_ref() else {
        return;
    };

    let bytes_per_row = DEFAULT_GLASSES_WIDTH * 4;

    while let Ok((glasses, left, right, lpos, rpos, rot)) = receiver.receiver.try_recv() {
        unsafe {
            let description = D3D11_TEXTURE2D_DESC {
                Width: DEFAULT_GLASSES_WIDTH,
                Height: DEFAULT_GLASSES_HEIGHT,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_TYPELESS,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };

            let left_data = D3D11_SUBRESOURCE_DATA {
                pSysMem: left.as_ptr() as *const c_void,
                SysMemPitch: bytes_per_row,
                SysMemSlicePitch: 0,
            };
            let right_data = D3D11_SUBRESOURCE_DATA {
                pSysMem: right.as_ptr() as *const c_void,
                SysMemPitch: bytes_per_row,
                SysMemSlicePitch: 0,
            };

            let mut left_tex: Option<ID3D11Texture2D> = None;
            let mut right_tex: Option<ID3D11Texture2D> = None;
            let _ = device.CreateTexture2D(&description, Some(&left_data), Some(&mut left_tex));
            let _ = device.CreateTexture2D(&description, Some(&right_data), Some(&mut right_tex));

            let Some(left_tex) = left_tex else { continue };
            let Some(right_tex) = right_tex else { continue };

            let start_y = -1.0 * (DEFAULT_GLASSES_FOV * 0.5 * std::f32::consts::PI / 180.0).tan();
            let start_x = start_y * (DEFAULT_GLASSES_WIDTH as f32 / DEFAULT_GLASSES_HEIGHT as f32);

            let frame_info = crate::bridge::ffi::T5_FrameInfo {
                leftTexHandle: left_tex.as_raw() as *mut c_void,
                rightTexHandle: right_tex.as_raw() as *mut c_void,
                texWidth_PIX: DEFAULT_GLASSES_WIDTH as u16,
                texHeight_PIX: DEFAULT_GLASSES_HEIGHT as u16,
                isSrgb: false,
                isUpsideDown: true,
                rotToLVC_GBD: rot.into(),
                posLVC_GBD: lpos.into(),
                rotToRVC_GBD: rot.into(),
                posRVC_GBD: rpos.into(),
                vci: T5_FrameInfo__bindgen_ty_1 {
                    startX_VCI: start_x,
                    startY_VCI: start_y,
                    width_VCI: -2.0 * start_x,
                    height_VCI: -2.0 * start_y,
                },
            };

            if let Err(e) = channels.client.send_frame_to_glasses(&glasses, &frame_info) {
                warn!("send_frame_to_glasses: {e}");
            }

            // Textures drop here. The SDK should have consumed them synchronously
            // during `send_frame_to_glasses`.
            let _ = (left_tex, right_tex);
            let _ = null::<u8>();
        }
    }
}
