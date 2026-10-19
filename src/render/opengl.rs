//! Linux OpenGL bridge: parallels `dx11.rs` for platforms where DX11 isn't
//! available. Creates a headless EGL/GL context, uploads the readback bytes
//! into GL textures, and submits them to the SDK via `t5SendFrameToGlasses`.
//!
//! Keeping a *separate* GL context (rather than reusing wgpu's native one)
//! mirrors the DX11 approach: wgpu on Linux usually runs on Vulkan, so there
//! is no GL context to share. The round trip is GPU → readback buffer → CPU
//! → `glTexImage2D`. Slow but correct, and the SDK handles the actual
//! display-side latency compensation.

use std::{
    ffi::{c_void, CString},
    ptr::null_mut,
};

use anyhow::{anyhow, Context, Result};
use bevy::{prelude::*, render::Render};
use glow::HasContext;
use khronos_egl as egl;

use crate::bridge::{DEFAULT_GLASSES_FOV, DEFAULT_GLASSES_HEIGHT, DEFAULT_GLASSES_WIDTH};
use crate::plugin::RenderChannels;
use crate::render::BufferReceiverResource;

pub struct OpenGlPlugin;

impl Plugin for OpenGlPlugin {
    fn build(&self, app: &mut App) {
        app.insert_non_send_resource(GlState::default())
            .add_systems(Render, (setup_gl, send_frames));
    }
}

#[derive(Default)]
struct GlState {
    initialised: bool,
    egl: Option<egl::DynamicInstance<egl::EGL1_4>>,
    display: Option<egl::Display>,
    context: Option<egl::Context>,
    surface: Option<egl::Surface>,
    gl: Option<glow::Context>,
}

fn setup_gl(mut state: NonSendMut<GlState>, mut channels: NonSendMut<RenderChannels>) {
    if state.initialised {
        return;
    }
    match init_egl_context() {
        Ok((egl_lib, display, context, surface, gl)) => {
            // The SDK takes the EGLContext handle as `void*` for the GL API.
            let raw = context.as_ptr() as *mut c_void;
            channels.client.set_opengl_graphics_context(raw);
            state.egl = Some(egl_lib);
            state.display = Some(display);
            state.context = Some(context);
            state.surface = Some(surface);
            state.gl = Some(gl);
            state.initialised = true;
        }
        Err(e) => error!("couldn't initialise EGL/GL context: {e:#}"),
    }
}

fn init_egl_context() -> Result<(
    egl::DynamicInstance<egl::EGL1_4>,
    egl::Display,
    egl::Context,
    egl::Surface,
    glow::Context,
)> {
    unsafe {
        let egl_lib = egl::DynamicInstance::<egl::EGL1_4>::load_required()
            .context("could not load libEGL")?;
        let display = egl_lib
            .get_display(egl::DEFAULT_DISPLAY)
            .ok_or_else(|| anyhow!("eglGetDisplay returned null"))?;
        egl_lib
            .initialize(display)
            .context("eglInitialize failed")?;
        egl_lib
            .bind_api(egl::OPENGL_API)
            .context("eglBindAPI(OPENGL) failed")?;

        let attribs = [
            egl::SURFACE_TYPE,
            egl::PBUFFER_BIT,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_BIT,
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            8,
            egl::NONE,
        ];
        let config = egl_lib
            .choose_first_config(display, &attribs)
            .context("eglChooseConfig failed")?
            .ok_or_else(|| anyhow!("no matching EGL config"))?;

        let ctx_attribs = [
            egl::CONTEXT_MAJOR_VERSION,
            3,
            egl::CONTEXT_MINOR_VERSION,
            3,
            egl::NONE,
        ];
        let context = egl_lib
            .create_context(display, config, None, &ctx_attribs)
            .context("eglCreateContext failed")?;

        let pbuf_attribs = [egl::WIDTH, 1, egl::HEIGHT, 1, egl::NONE];
        let surface = egl_lib
            .create_pbuffer_surface(display, config, &pbuf_attribs)
            .context("eglCreatePbufferSurface failed")?;

        egl_lib
            .make_current(display, Some(surface), Some(surface), Some(context))
            .context("eglMakeCurrent failed")?;

        let gl = glow::Context::from_loader_function(|name| {
            let c = CString::new(name).unwrap();
            egl_lib
                .get_proc_address(c.to_str().unwrap())
                .map(|p| p as *const _)
                .unwrap_or(null_mut())
        });

        Ok((egl_lib, display, context, surface, gl))
    }
}

fn send_frames(
    state: NonSend<GlState>,
    channels: NonSend<RenderChannels>,
    receiver: NonSend<BufferReceiverResource>,
) {
    let (Some(_egl), Some(gl)) = (state.egl.as_ref(), state.gl.as_ref()) else {
        return;
    };

    while let Ok((glasses, left, right, lpos, rpos, rot)) = receiver.receiver.try_recv() {
        unsafe {
            let left_tex = upload_eye_texture(gl, &left);
            let right_tex = upload_eye_texture(gl, &right);
            let (Some(left_tex), Some(right_tex)) = (left_tex, right_tex) else {
                continue;
            };

            let start_y = -(DEFAULT_GLASSES_FOV * 0.5 * std::f32::consts::PI / 180.0).tan();
            let start_x = start_y * (DEFAULT_GLASSES_WIDTH as f32 / DEFAULT_GLASSES_HEIGHT as f32);

            let frame_info = crate::bridge::ffi::T5_FrameInfo {
                // The SDK on GL expects the GLuint cast to void*.
                leftTexHandle: left_tex.0.get() as usize as *mut c_void,
                rightTexHandle: right_tex.0.get() as usize as *mut c_void,
                texWidth_PIX: DEFAULT_GLASSES_WIDTH as u16,
                texHeight_PIX: DEFAULT_GLASSES_HEIGHT as u16,
                isSrgb: false,
                isUpsideDown: true,
                rotToLVC_GBD: rot.into(),
                posLVC_GBD: lpos.into(),
                rotToRVC_GBD: rot.into(),
                posRVC_GBD: rpos.into(),
                vci: crate::bridge::ffi::T5_FrameInfo__bindgen_ty_1 {
                    startX_VCI: start_x,
                    startY_VCI: start_y,
                    width_VCI: -2.0 * start_x,
                    height_VCI: -2.0 * start_y,
                },
            };

            if let Err(e) = channels.client.send_frame_to_glasses(&glasses, &frame_info) {
                warn!("send_frame_to_glasses: {e}");
            }

            gl.delete_texture(left_tex);
            gl.delete_texture(right_tex);
        }
    }
}

unsafe fn upload_eye_texture(gl: &glow::Context, bytes: &[u8]) -> Option<glow::NativeTexture> {
    let tex = gl.create_texture().ok()?;
    gl.bind_texture(glow::TEXTURE_2D, Some(tex));
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MIN_FILTER,
        glow::LINEAR as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MAG_FILTER,
        glow::LINEAR as i32,
    );
    gl.tex_image_2d(
        glow::TEXTURE_2D,
        0,
        glow::RGBA8 as i32,
        DEFAULT_GLASSES_WIDTH as i32,
        DEFAULT_GLASSES_HEIGHT as i32,
        0,
        glow::RGBA,
        glow::UNSIGNED_BYTE,
        Some(bytes),
    );
    gl.bind_texture(glow::TEXTURE_2D, None);
    Some(tex)
}
