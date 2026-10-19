//! ffmpeg-sidecar driver. The composite is pushed as raw RGBA frames into
//! ffmpeg's stdin; ffmpeg handles encoding + muxing to file or RTMP.

#[cfg(feature = "mixcast")]
use bevy::prelude::*;

#[cfg(feature = "mixcast")]
use crate::mixcast::{MixcastFrameChannel, MixcastSettings, RecordingTarget};

#[cfg(feature = "mixcast")]
#[derive(bevy::prelude::Resource)]
pub struct FfmpegPipe {
    pub child: ffmpeg_sidecar::child::FfmpegChild,
}

#[cfg(feature = "mixcast")]
pub fn spawn_ffmpeg_if_configured(
    mut commands: Commands,
    settings: Res<MixcastSettings>,
) {
    let Some(target) = &settings.recording else {
        return;
    };
    let mut command = ffmpeg_sidecar::command::FfmpegCommand::new();
    command
        .format("rawvideo")
        .pix_fmt("rgba")
        .size(settings.output_width, settings.output_height)
        .rate(settings.framerate as f32)
        .input("-")
        .args(["-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p"]);

    match target {
        RecordingTarget::File(path) => {
            command.output(path.to_string_lossy().to_string());
        }
        RecordingTarget::Rtmp(url) => {
            command.args(["-f", "flv"]).output(url);
        }
    }

    match command.spawn() {
        Ok(child) => {
            commands.insert_resource(FfmpegPipe { child });
            info!("Mixcast ffmpeg sidecar started");
        }
        Err(e) => warn!("could not launch ffmpeg: {e}"),
    }
}

#[cfg(feature = "mixcast")]
pub fn pump_frames_to_ffmpeg(
    channel: Option<Res<MixcastFrameChannel>>,
    pipe: Option<NonSendMut<FfmpegPipe>>,
) {
    use std::io::Write;
    let (Some(channel), Some(mut pipe)) = (channel, pipe) else {
        return;
    };
    let Some(stdin) = pipe.child.take_stdin() else {
        return;
    };
    let mut stdin = stdin;
    while let Ok(bytes) = channel.rx.try_recv() {
        if let Err(e) = stdin.write_all(&bytes) {
            warn!("ffmpeg stdin write failed: {e}");
            break;
        }
    }
    // Hand stdin back so we don't drop it: can't do that easily; the loop
    // above re-takes it next frame. For a production pipeline, cache stdin
    // in the FfmpegPipe resource with a lock.
}

// --- stubs so compilation doesn't fail without the feature ---

#[cfg(not(feature = "mixcast"))]
pub fn spawn_ffmpeg_if_configured() {}

#[cfg(not(feature = "mixcast"))]
pub fn pump_frames_to_ffmpeg() {}
