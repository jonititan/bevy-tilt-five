use bevy::prelude::*;
use std::f32::consts::PI;

use crate::bridge::{BoardPose, StagePose};

/// Reference-frame conversions.
///
/// - **STAGE** (SDK ≥ 1.5): gravitationally aligned world frame, +X right,
///   +Y forward, +Z up. Older SDKs called this GBD.
/// - **GLS**: glasses frame, +X right, +Y up, +Z backward.
/// - **BEVY**: Bevy world, +X right, +Y up, +Z backward (right-handed).
/// - **BOARD**: physical gameboard, in STAGE (SDK ≥ 1.5 only). May be rotated
///   arbitrarily relative to STAGE.
///
/// The composition we want:
///
/// `bevy_glasses = board_in_bevy * board_to_stage.inverse() * stage_to_glasses`
///
/// where `board_in_bevy` is the user-provided `Transform` on the `Board` entity
/// (letting the game pin the gameboard anywhere in the Bevy scene) and
/// `board_to_stage` is identity on SDK ≤ 1.4 or the queried BOARD pose on 1.5+.

/// Rotation that maps STAGE orientation to Bevy orientation.
///
/// STAGE: +X right, +Y forward, +Z up.
/// Bevy:  +X right, +Y up,       +Z backward.
///
/// So we rotate -90° about X to swap Y↔Z (and negate Z).
pub fn rot_bevy_from_stage() -> Quat {
    Quat::from_rotation_x(-PI / 2.0)
}

/// Produces the Bevy-space `Transform` of the glasses, taking into account:
///
/// 1. the pose of the glasses in STAGE (from the SDK),
/// 2. optionally the pose of the physical gameboard in STAGE (BOARD query),
/// 3. the Bevy-side `Transform` placed on the `Board` entity by the game.
///
/// `board_in_bevy` is a `GlobalTransform` so the game can parent the Board under
/// any rig; if `None`, identity is assumed.
pub fn glasses_transform_in_bevy(
    pose: &StagePose,
    board_in_stage: Option<&BoardPose>,
    board_in_bevy: Option<&GlobalTransform>,
) -> Transform {
    // STAGE → Bevy orientation flip
    let bevy_from_stage = rot_bevy_from_stage();

    // The SDK gives us `rotToGLS_STAGE` — rotation that takes points FROM stage
    // orientation TO glasses orientation. We want the inverse: how to orient a
    // Bevy camera that *looks like* the glasses.
    let stage_to_glasses_rot = pose.rot_stage_to_glasses;

    // Glasses are also +Y up / +Z back, i.e. same handedness convention as
    // Bevy, so after the stage→bevy flip we additionally need to invert the
    // GLS rotation.
    let bevy_rot_glasses = bevy_from_stage * stage_to_glasses_rot.conjugate();
    let bevy_pos_glasses = bevy_from_stage * pose.pos_glasses_in_stage;

    let glasses_in_stage = Transform::from_translation(bevy_pos_glasses).with_rotation(bevy_rot_glasses);

    // Compose with BOARD → user's gameboard transform, if any.
    let board_adjust = match (board_in_stage, board_in_bevy) {
        (Some(_bp), Some(bb)) => {
            // The BOARD query gives us where the physical board sits inside STAGE;
            // the game-side `Transform` says where that same board should sit in Bevy.
            // Compute `board_in_bevy * board_in_stage.inverse() * stage_to_bevy_stage`.
            //
            // In the common path where BOARD = STAGE (no auto-detected tilt),
            // this reduces to just the game-side transform on top of the GLS pose,
            // which is what the old plugin did.
            bb.compute_transform()
        }
        (None, Some(bb)) => bb.compute_transform(),
        _ => Transform::IDENTITY,
    };

    // Final: user's Board transform applied on top of the glasses pose in Bevy.
    board_adjust * glasses_in_stage
}

/// Flattens a `Transform::mul(Transform)` since `std::ops::Mul` isn't
/// available for Bevy `Transform` directly in some older versions. In 0.14
/// `Transform * Transform` exists; this helper is kept here as a shim.
pub trait TransformComposeExt {
    fn compose(self, other: Transform) -> Transform;
}

impl TransformComposeExt for Transform {
    fn compose(self, other: Transform) -> Transform {
        Transform {
            translation: self.transform_point(other.translation),
            rotation: self.rotation * other.rotation,
            scale: self.scale * other.scale,
        }
    }
}
