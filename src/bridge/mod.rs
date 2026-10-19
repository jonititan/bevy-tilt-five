pub mod ffi {
    #![allow(non_upper_case_globals)]
    #![allow(non_camel_case_types)]
    #![allow(non_snake_case)]
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

use std::{
    collections::HashMap,
    ffi::{c_char, c_void, CStr, CString},
    fmt::Display,
    mem::MaybeUninit,
    thread,
    time::Duration,
};

use bevy::prelude::{Quat, Reflect, Vec2, Vec3};
use ffi::*;

use thiserror::Error;

pub type Result<T, E = BridgeError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("tilt-five service error: 0x{0:08X}")]
    Service(u32),
    #[error("tilt-five library not loaded: {0}")]
    Load(String),
    #[error("string contained interior NUL")]
    Nul,
    #[error("glasses handle not found")]
    UnknownGlasses,
    #[error("operation not supported by linked SDK (needs 1.5.0+)")]
    Unsupported,
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("other: {0}")]
    Other(String),
}

impl From<std::ffi::NulError> for BridgeError {
    fn from(_: std::ffi::NulError) -> Self {
        BridgeError::Nul
    }
}

/// Opaque identifier for a pair of glasses, as reported by the service.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Reflect)]
pub struct Glasses(String);

impl From<String> for Glasses {
    fn from(v: String) -> Self {
        Glasses(v)
    }
}
impl From<&str> for Glasses {
    fn from(v: &str) -> Self {
        Glasses(v.to_string())
    }
}
impl From<Glasses> for String {
    fn from(v: Glasses) -> Self {
        v.0
    }
}
impl<'a> From<&'a Glasses> for &'a str {
    fn from(v: &'a Glasses) -> Self {
        &v.0
    }
}
impl From<&Glasses> for String {
    fn from(v: &Glasses) -> Self {
        v.0.clone()
    }
}
impl Display for Glasses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Identifier for a wand on a specific pair of glasses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Reflect)]
pub struct WandId(pub u8);

pub const DEFAULT_GLASSES_WIDTH: u32 = 1216;
pub const DEFAULT_GLASSES_HEIGHT: u32 = 768;
pub const DEFAULT_GLASSES_FOV: f32 = 48.0;

/// Minimum camera buffer dimensions required by the SDK.
pub const T5_MIN_CAM_IMAGE_WIDTH: u32 = 768;
pub const T5_MIN_CAM_IMAGE_HEIGHT: u32 = 600;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Reflect)]
pub enum T5GameboardType {
    None = 1,
    LE = 2,
    XE = 3,
    XeRaised = 4,
}

impl T5GameboardType {
    pub fn from_raw(v: i32) -> Self {
        match v {
            2 => T5GameboardType::LE,
            3 => T5GameboardType::XE,
            4 => T5GameboardType::XeRaised,
            _ => T5GameboardType::None,
        }
    }
}

/// Which pose to request: in-headset (predicted) or spectator (smoother, for
/// third-person compositing). Mapped to `T5_GlassesPoseUsage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoseUsage {
    Glasses = 1,
    Spectator = 2,
}

/// Pose of the glasses in the STAGE reference frame (what the SDK ≤ 1.4 called GBD).
///
/// The STAGE frame is gravitationally aligned: +Z up, +X right, +Y forward.
#[derive(Clone, Copy, Debug)]
pub struct StagePose {
    pub timestamp_nanos: u64,
    pub pos_glasses_in_stage: Vec3,
    pub rot_stage_to_glasses: Quat,
    pub gameboard_type: T5GameboardType,
}

/// Pose of the physical gameboard in the STAGE reference frame.
/// Only populated when the Kindly Kraken (1.5+) BOARD query is available.
#[derive(Clone, Copy, Debug, Default)]
pub struct BoardPose {
    pub pos_board_in_stage: Vec3,
    pub rot_stage_to_board: Quat,
}

#[derive(Clone, Copy, Debug)]
pub struct WandReport {
    pub timestamp_nanos: u64,
    pub analog_valid: bool,
    pub battery_valid: bool,
    pub buttons_valid: bool,
    pub pose_valid: bool,
    pub trigger: f32,
    pub stick: Vec2,
    pub battery: u8,
    pub buttons: WandButtons,
    pub rot_stage_to_wand: Quat,
    pub pos_aim_in_stage: Vec3,
    pub pos_fingertips_in_stage: Vec3,
    pub pos_grip_in_stage: Vec3,
    pub hand: WandHand,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WandButtons {
    pub t5: bool,
    pub one: bool,
    pub two: bool,
    pub three: bool,
    pub a: bool,
    pub b: bool,
    pub x: bool,
    pub y: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WandHand {
    #[default]
    Unknown,
    Left,
    Right,
}

impl WandHand {
    fn from_raw(v: u32) -> Self {
        match v {
            1 => WandHand::Left,
            2 => WandHand::Right,
            _ => WandHand::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum WandStreamEvent {
    Connect(WandId),
    Disconnect(WandId),
    Desync(WandId),
    Report(WandId, WandReport),
}

/// A camera frame received from a glasses onboard camera.
#[derive(Clone, Debug)]
pub struct CameraFrame {
    pub camera_index: u8,
    pub width: u16,
    pub height: u16,
    pub stride: u16,
    pub illumination_mode: u8,
    pub pos_cam_in_stage: Vec3,
    pub rot_stage_to_cam: Quat,
    pub pixels: Vec<u8>,
}

/// Platform-specific dynamic library filename.
fn dynamic_library_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "TiltFiveNative.dll"
    }
    #[cfg(target_os = "linux")]
    {
        "libTiltFiveNative.so"
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        compile_error!("unsupported platform for TiltFiveNative")
    }
}

/// Retry wrapper for the SDK's `kT5_Error_NoService` transient. Non-service
/// errors return immediately with the raw code.
fn op<T, F>(mut f: F, max_attempts: usize) -> Result<T>
where
    F: FnMut() -> (u32, T),
{
    let mut attempts = 0usize;
    loop {
        let (err, value) = f();
        if err == 0 {
            return Ok(value);
        }
        if err == T5_ERROR_NO_SERVICE && attempts < max_attempts {
            thread::sleep(Duration::from_millis(10));
            attempts += 1;
            continue;
        }
        return Err(BridgeError::Service(err));
    }
}

fn op_unit<F>(f: F, max_attempts: usize) -> Result<()>
where
    F: FnMut() -> u32,
{
    let mut f = f;
    op(move || (f(), ()), max_attempts)
}

/// The handful of camera buffers we keep pooled per-glasses.
const CAMERA_POOL_SIZE: usize = 4;
const CAMERA_BUFFER_BYTES: usize =
    (T5_MIN_CAM_IMAGE_WIDTH as usize) * (T5_MIN_CAM_IMAGE_HEIGHT as usize);

struct CameraBuffer {
    bytes: Box<[u8]>,
}

pub struct T5Client {
    app: String,
    bridge: TiltFiveNative,
    ctx: T5_Context,
    glasses: HashMap<Glasses, T5_Glasses>,
    graphics_context: Option<(T5_GraphicsApi, *mut c_void)>,
    cam_pools: HashMap<Glasses, Vec<CameraBuffer>>,
}

/// Safety: all handle access is single-threaded via `NonSend` in Bevy.
unsafe impl Send for T5Client {}

impl T5Client {
    pub fn new<T: Into<String>, R: Into<String>>(app: T, version: R) -> Result<T5Client> {
        unsafe {
            let app: String = app.into();
            let version: String = version.into();
            let app_id = CString::new(app.clone())?;
            let version = CString::new(version)?;

            let bridge = TiltFiveNative::new(dynamic_library_name())
                .map_err(|e| BridgeError::Load(e.to_string()))?;

            let mut ctx = MaybeUninit::uninit();
            let mut platform = MaybeUninit::uninit();
            let info = T5_ClientInfo {
                applicationId: app_id.as_ptr(),
                applicationVersion: version.as_ptr(),
                sdkType: 0u8,
                reserved: 0u64,
            };

            op_unit(
                || bridge.t5CreateContext(ctx.as_mut_ptr(), &info, platform.as_mut_ptr()),
                100,
            )?;

            Ok(T5Client {
                app,
                bridge,
                ctx: ctx.assume_init(),
                glasses: Default::default(),
                graphics_context: None,
                cam_pools: Default::default(),
            })
        }
    }

    pub fn get_gameboard_size(&self, kind: T5GameboardType) -> Result<T5_GameboardSize> {
        unsafe {
            let mut out = MaybeUninit::uninit();
            op_unit(
                || self.bridge.t5GetGameboardSize(self.ctx, kind as u32, out.as_mut_ptr()),
                100,
            )?;
            Ok(out.assume_init())
        }
    }

    pub fn list_glasses(&self) -> Result<Vec<Glasses>> {
        unsafe {
            let mut buffer = [c_char::MIN; 1024];
            let mut buffer_size: usize = 1024;
            op_unit(
                || self.bridge.t5ListGlasses(self.ctx, buffer.as_mut_ptr(), &mut buffer_size),
                1,
            )?;

            let mut tmp = vec![];
            let start = buffer.as_ptr();
            let mut offset = 0usize;
            loop {
                let ptr = start.add(offset);
                let s = CStr::from_ptr(ptr);
                offset += s.to_bytes_with_nul().len();
                let value = s.to_str();
                match value {
                    Ok(v) if !v.is_empty() => tmp.push(Glasses::from(v)),
                    _ => break,
                }
            }
            Ok(tmp)
        }
    }

    pub fn create_glasses(&mut self, glasses_id: &Glasses) -> Result<(Glasses, Option<String>)> {
        unsafe {
            let id_s: &str = glasses_id.into();
            let id_c = CString::new(id_s)?;
            let mut handle = MaybeUninit::uninit();
            op_unit(
                || self.bridge.t5CreateGlasses(self.ctx, id_c.as_ptr(), handle.as_mut_ptr()),
                100,
            )?;
            let handle = handle.assume_init();

            let display = CString::new(format!("{} - {}", self.app, glasses_id))?;
            op_unit(|| self.bridge.t5ReserveGlasses(handle, display.as_ptr()), 100)?;
            op_unit(|| self.bridge.t5EnsureGlassesReady(handle), 100)?;

            if let Some((api, ctx)) = self.graphics_context {
                op_unit(
                    || self.bridge.t5InitGlassesGraphicsContext(handle, api, ctx),
                    100,
                )?;
            }

            self.glasses.insert(glasses_id.clone(), handle);

            let friendly = self.get_glasses_name(glasses_id).ok();
            Ok((glasses_id.clone(), friendly))
        }
    }

    pub fn release_glasses(&mut self, glasses: &Glasses) -> Result<()> {
        if let Some(handle) = self.glasses.remove(glasses) {
            self.cam_pools.remove(glasses);
            unsafe {
                // best-effort disable streams before release
                let wand_off = T5_WandStreamConfig { enabled: false };
                let _ = op_unit(
                    || self.bridge.t5ConfigureWandStreamForGlasses(handle, &wand_off),
                    1,
                );
                for idx in 0..=1u8 {
                    let cam_off = T5_CameraStreamConfig { cameraIndex: idx, enabled: false };
                    let _ = op_unit(
                        || self.bridge.t5ConfigureCameraStreamForGlasses(handle, cam_off),
                        1,
                    );
                }
                op_unit(|| self.bridge.t5ReleaseGlasses(handle), 100)
            }
        } else {
            Ok(())
        }
    }

    /// Returns the glasses pose in the STAGE reference frame.
    pub fn get_glasses_pose(&self, glasses: &Glasses, usage: PoseUsage) -> Result<StagePose> {
        let handle = *self.glasses.get(glasses).ok_or(BridgeError::UnknownGlasses)?;
        unsafe {
            let mut pose = MaybeUninit::uninit();
            op_unit(
                || self.bridge.t5GetGlassesPose(handle, usage as u32, pose.as_mut_ptr()),
                1,
            )?;
            let raw = pose.assume_init();
            Ok(StagePose {
                timestamp_nanos: raw.timestampNanos,
                pos_glasses_in_stage: raw.posGLS_GBD.into(),
                rot_stage_to_glasses: raw.rotToGLS_GBD.into(),
                gameboard_type: T5GameboardType::from_raw(raw.gameboardType as i32),
            })
        }
    }

    /// 1.5.0+ only. Returns the physical gameboard pose in STAGE. On older SDKs
    /// returns `Err(Unsupported)`.
    pub fn get_board_pose(&self, _glasses: &Glasses) -> Result<BoardPose> {
        #[cfg(feature = "sdk-1-5")]
        {
            // Kindly Kraken header introduces `t5GetBoardPose(T5_Glasses, T5_BoardPose*)`
            // and a `T5_BoardPose { Vec3 posBOARD_STAGE; Quat rotToBOARD_STAGE; }`.
            // When the user swaps in the real headers, uncomment the body below and the
            // bindgen allowlist will pick it up.
            //
            // let handle = *self.glasses.get(_glasses).ok_or(BridgeError::UnknownGlasses)?;
            // unsafe {
            //     let mut out = MaybeUninit::uninit();
            //     op_unit(|| self.bridge.t5GetBoardPose(handle, out.as_mut_ptr()), 1)?;
            //     let raw = out.assume_init();
            //     Ok(BoardPose {
            //         pos_board_in_stage: raw.posBOARD_STAGE.into(),
            //         rot_stage_to_board: raw.rotToBOARD_STAGE.into(),
            //     })
            // }
            Err(BridgeError::Unsupported)
        }
        #[cfg(not(feature = "sdk-1-5"))]
        {
            Err(BridgeError::Unsupported)
        }
    }

    pub fn configure_wand_stream(&self, glasses: &Glasses, enabled: bool) -> Result<()> {
        let handle = *self.glasses.get(glasses).ok_or(BridgeError::UnknownGlasses)?;
        unsafe {
            let cfg = T5_WandStreamConfig { enabled };
            op_unit(|| self.bridge.t5ConfigureWandStreamForGlasses(handle, &cfg), 1)
        }
    }

    /// Non-blocking drain of pending wand stream events.
    pub fn drain_wand_events(&self, glasses: &Glasses) -> Result<Vec<WandStreamEvent>> {
        let handle = *self.glasses.get(glasses).ok_or(BridgeError::UnknownGlasses)?;
        let mut events = Vec::new();
        unsafe {
            loop {
                let mut raw = MaybeUninit::<T5_WandStreamEvent>::uninit();
                let err = self
                    .bridge
                    .t5ReadWandStreamForGlasses(handle, raw.as_mut_ptr(), 0);
                if err != 0 {
                    // Timeout / no-data is the common exit; treat it as "stream empty".
                    break;
                }
                let raw = raw.assume_init();
                let id = WandId(raw.wandId);
                let evt = match raw.type_ {
                    1 => WandStreamEvent::Connect(id),
                    2 => WandStreamEvent::Disconnect(id),
                    3 => WandStreamEvent::Desync(id),
                    4 => WandStreamEvent::Report(id, decode_wand_report(&raw.report)),
                    _ => continue,
                };
                events.push(evt);
            }
        }
        Ok(events)
    }

    pub fn configure_camera_stream(
        &mut self,
        glasses: &Glasses,
        camera_index: u8,
        enabled: bool,
    ) -> Result<()> {
        let handle = *self.glasses.get(glasses).ok_or(BridgeError::UnknownGlasses)?;
        unsafe {
            let cfg = T5_CameraStreamConfig { cameraIndex: camera_index, enabled };
            op_unit(
                || self.bridge.t5ConfigureCameraStreamForGlasses(handle, cfg),
                1,
            )?;
        }
        if enabled {
            let needed =
                CAMERA_POOL_SIZE.saturating_sub(self.cam_pools.get(glasses).map_or(0, Vec::len));
            let mut new_buffers = Vec::with_capacity(needed);
            for _ in 0..needed {
                let buf = CameraBuffer {
                    bytes: vec![0u8; CAMERA_BUFFER_BYTES].into_boxed_slice(),
                };
                self.submit_empty_internal(handle, camera_index, &buf)?;
                new_buffers.push(buf);
            }
            self.cam_pools
                .entry(glasses.clone())
                .or_default()
                .extend(new_buffers);
        }
        Ok(())
    }

    fn submit_empty_internal(
        &self,
        handle: T5_Glasses,
        camera_index: u8,
        buf: &CameraBuffer,
    ) -> Result<()> {
        unsafe {
            let mut img = T5_CamImage {
                imageWidth: 0,
                imageHeight: 0,
                imageStride: 0,
                cameraIndex: camera_index,
                illuminationMode: 0,
                bufferSize: buf.bytes.len() as u32,
                pixelData: buf.bytes.as_ptr() as *mut u8,
                posCAM_GBD: T5_Vec3 { x: 0.0, y: 0.0, z: 0.0 },
                rotToCAM_GBD: T5_Quat { w: 1.0, x: 0.0, y: 0.0, z: 0.0 },
            };
            op_unit(
                || self.bridge.t5SubmitEmptyCamImageBuffer(handle, &mut img),
                1,
            )
        }
    }

    /// Drain any filled camera-frame buffers and resubmit them as empty.
    pub fn drain_camera_frames(&mut self, glasses: &Glasses) -> Result<Vec<CameraFrame>> {
        let handle = *self.glasses.get(glasses).ok_or(BridgeError::UnknownGlasses)?;
        let mut out = Vec::new();
        unsafe {
            loop {
                let mut img = MaybeUninit::<T5_CamImage>::uninit();
                let err = self
                    .bridge
                    .t5GetFilledCamImageBuffer(handle, img.as_mut_ptr());
                if err != 0 {
                    break;
                }
                let img = img.assume_init();
                let len = img.imageStride as usize * img.imageHeight as usize;
                if img.pixelData.is_null() || len == 0 {
                    continue;
                }
                let pixels = std::slice::from_raw_parts(img.pixelData, len).to_vec();
                out.push(CameraFrame {
                    camera_index: img.cameraIndex,
                    width: img.imageWidth,
                    height: img.imageHeight,
                    stride: img.imageStride,
                    illumination_mode: img.illuminationMode,
                    pos_cam_in_stage: img.posCAM_GBD.into(),
                    rot_stage_to_cam: img.rotToCAM_GBD.into(),
                    pixels,
                });

                // resubmit the buffer so the service can keep streaming
                let mut resubmit = img;
                resubmit.imageWidth = 0;
                resubmit.imageHeight = 0;
                resubmit.imageStride = 0;
                let _ = self
                    .bridge
                    .t5SubmitEmptyCamImageBuffer(handle, &mut resubmit);
            }
        }
        Ok(out)
    }

    /// # Safety
    /// `info` must point to a valid T5_FrameInfo with texture handles that
    /// remain live until the SDK returns.
    pub unsafe fn send_frame_to_glasses(
        &self,
        id: &Glasses,
        info: *const T5_FrameInfo,
    ) -> Result<()> {
        let handle = *self.glasses.get(id).ok_or(BridgeError::UnknownGlasses)?;
        op_unit(|| self.bridge.t5SendFrameToGlasses(handle, info), 1)
    }

    pub fn set_dx11_graphics_context(&mut self, device: *mut c_void) {
        self.graphics_context = Some((T5_GraphicsApi_kT5_GraphicsApi_D3D11, device));
    }

    pub fn set_opengl_graphics_context(&mut self, context: *mut c_void) {
        self.graphics_context = Some((T5_GraphicsApi_kT5_GraphicsApi_GL, context));
    }

    pub fn get_ipd(&self, id: &Glasses) -> Result<f32> {
        let handle = *self.glasses.get(id).ok_or(BridgeError::UnknownGlasses)?;
        unsafe {
            let mut ipd = MaybeUninit::uninit();
            op_unit(
                || {
                    self.bridge.t5GetGlassesFloatParam(
                        handle,
                        0,
                        T5_ParamGlasses_kT5_ParamGlasses_Float_IPD,
                        ipd.as_mut_ptr(),
                    )
                },
                1,
            )?;
            Ok(ipd.assume_init() as f32)
        }
    }

    pub fn get_glasses_name(&self, id: &Glasses) -> Result<String> {
        let handle = *self.glasses.get(id).ok_or(BridgeError::UnknownGlasses)?;
        unsafe {
            let mut buffer = [c_char::MIN; 1024];
            let mut buffer_size: usize = 1024;
            op_unit(
                || {
                    self.bridge.t5GetGlassesUtf8Param(
                        handle,
                        0,
                        T5_ParamGlasses_kT5_ParamGlasses_UTF8_FriendlyName,
                        buffer.as_mut_ptr(),
                        &mut buffer_size,
                    )
                },
                1,
            )?;
            let s = CStr::from_ptr(buffer.as_ptr()).to_str().unwrap_or("").to_string();
            if s.is_empty() {
                Err(BridgeError::Other("empty name".into()))
            } else {
                Ok(s)
            }
        }
    }
}

impl Drop for T5Client {
    fn drop(&mut self) {
        unsafe {
            self.bridge.t5DestroyContext(&mut self.ctx);
        }
    }
}

fn decode_wand_report(raw: &T5_WandReport) -> WandReport {
    WandReport {
        timestamp_nanos: raw.timestampNanos,
        analog_valid: raw.analogValid,
        battery_valid: raw.batteryValid,
        buttons_valid: raw.buttonsValid,
        pose_valid: raw.poseValid,
        trigger: raw.trigger,
        stick: Vec2::new(raw.stick.x, raw.stick.y),
        battery: raw.battery,
        buttons: WandButtons {
            t5: raw.buttons.t5,
            one: raw.buttons.one,
            two: raw.buttons.two,
            three: raw.buttons.three,
            a: raw.buttons.a,
            b: raw.buttons.b,
            x: raw.buttons.x,
            y: raw.buttons.y,
        },
        rot_stage_to_wand: raw.rotToWND_GBD.into(),
        pos_aim_in_stage: raw.posAim_GBD.into(),
        pos_fingertips_in_stage: raw.posFingertips_GBD.into(),
        pos_grip_in_stage: raw.posGrip_GBD.into(),
        hand: WandHand::from_raw(raw.hand),
    }
}

impl From<T5_Vec2> for Vec2 {
    fn from(v: T5_Vec2) -> Self {
        Vec2::new(v.x, v.y)
    }
}
impl From<T5_Vec3> for Vec3 {
    fn from(v: T5_Vec3) -> Self {
        Vec3::new(v.x, v.y, v.z)
    }
}
impl From<T5_Quat> for Quat {
    fn from(v: T5_Quat) -> Self {
        Quat::from_xyzw(v.x, v.y, v.z, v.w)
    }
}
impl From<Vec2> for T5_Vec2 {
    fn from(v: Vec2) -> Self {
        T5_Vec2 { x: v.x, y: v.y }
    }
}
impl From<Vec3> for T5_Vec3 {
    fn from(v: Vec3) -> Self {
        T5_Vec3 { x: v.x, y: v.y, z: v.z }
    }
}
impl From<Quat> for T5_Quat {
    fn from(v: Quat) -> Self {
        T5_Quat { x: v.x, y: v.y, z: v.z, w: v.w }
    }
}
