//! Wand input: pumps `t5ReadWandStreamForGlasses`, maps the SDK stream to
//! Bevy events, and spawns a `Wand` entity per connected wand with
//! components for pose, buttons, trigger, stick, hand, and battery.
//!
//! Wand reports are issued in the STAGE reference frame. The wand entity is
//! parented under the Board entity so arbitrary gameboard orientation falls
//! out naturally: the game-side `Transform` on the Board moves wands too.

use bevy::{prelude::*, render::RenderApp, utils::HashMap};
use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::{
    bridge::{Glasses, WandButtons, WandHand, WandId, WandReport, WandStreamEvent},
    conversions::rot_bevy_from_stage,
    plugin::{Board, RenderChannels},
};

/// Marker + identity for a per-wand entity.
#[derive(Component, Debug, Clone)]
pub struct Wand {
    pub glasses: Glasses,
    pub id: WandId,
    pub hand: WandHand,
    pub battery: u8,
}

/// Which glasses "owns" this wand. Mirrors `Wand::glasses` so queries can
/// filter by owner cheaply via `With<WandOwner>` idiom.
#[derive(Component, Debug, Clone)]
pub struct WandOwner(pub Glasses);

/// Analog inputs on the wand.
#[derive(Component, Debug, Default, Clone, Copy)]
pub struct WandAnalog {
    pub trigger: f32,
    pub stick: Vec2,
}

/// Button state on the wand (mirrors T5_WandReport.buttons).
#[derive(Component, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WandButtonsState {
    pub t5: bool,
    pub one: bool,
    pub two: bool,
    pub three: bool,
    pub a: bool,
    pub b: bool,
    pub x: bool,
    pub y: bool,
}

impl From<WandButtons> for WandButtonsState {
    fn from(b: WandButtons) -> Self {
        Self {
            t5: b.t5,
            one: b.one,
            two: b.two,
            three: b.three,
            a: b.a,
            b: b.b,
            x: b.x,
            y: b.y,
        }
    }
}

/// Fine-grained Bevy events emitted whenever the wand stream changes state.
#[derive(Event, Debug, Clone)]
pub enum WandEvent {
    Connected { glasses: Glasses, wand: WandId },
    Disconnected { glasses: Glasses, wand: WandId },
    Desync { glasses: Glasses, wand: WandId },
    Report { glasses: Glasses, wand: WandId, report: WandReport },
}

#[derive(Resource)]
struct WandChannelMain {
    rx: Receiver<InternalWandEvent>,
}

struct WandChannelRender {
    tx: Sender<InternalWandEvent>,
}

#[derive(Debug, Clone)]
enum InternalWandEvent {
    Connected(Glasses, WandId),
    Disconnected(Glasses, WandId),
    Desync(Glasses, WandId),
    Report(Glasses, WandId, WandReport),
}

/// Map `(glasses, wand_id) → entity` to update components on repeated reports.
#[derive(Resource, Default)]
struct WandEntities(HashMap<(Glasses, WandId), Entity>);

pub struct WandPlugin;

impl Plugin for WandPlugin {
    fn build(&self, app: &mut App) {
        let (tx, rx) = unbounded::<InternalWandEvent>();
        app.add_event::<WandEvent>()
            .insert_resource(WandChannelMain { rx })
            .init_resource::<WandEntities>()
            .add_systems(Update, pump_wand_events_main);

        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.add_systems(ExtractSchedule, pump_wand_events_render);
            render_app
                .world_mut()
                .insert_non_send_resource(WandChannelRender { tx });
        }
    }
}

/// Render-app: drain the SDK wand stream for every connected pair of glasses.
fn pump_wand_events_render(
    channels: NonSend<RenderChannels>,
    render_ch: NonSendMut<WandChannelRender>,
    list: Res<crate::plugin::GlassesRenderList>,
) {
    for gid in list.0.keys() {
        match channels.client.drain_wand_events(gid) {
            Ok(events) => {
                for evt in events {
                    let internal = match evt {
                        WandStreamEvent::Connect(w) => InternalWandEvent::Connected(gid.clone(), w),
                        WandStreamEvent::Disconnect(w) => {
                            InternalWandEvent::Disconnected(gid.clone(), w)
                        }
                        WandStreamEvent::Desync(w) => InternalWandEvent::Desync(gid.clone(), w),
                        WandStreamEvent::Report(w, r) => {
                            InternalWandEvent::Report(gid.clone(), w, r)
                        }
                    };
                    let _ = render_ch.tx.send(internal);
                }
            }
            Err(_) => {}
        }
    }
}

/// Main-app: drain the cross-world channel, maintain Wand entities, and emit
/// public `WandEvent`s for game code to react to.
fn pump_wand_events_main(
    mut commands: Commands,
    mut entities: ResMut<WandEntities>,
    channels: Res<WandChannelMain>,
    mut events: EventWriter<WandEvent>,
    board: Query<Entity, With<Board>>,
    mut wand_q: Query<(&mut Transform, &mut WandAnalog, &mut WandButtonsState, &mut Wand)>,
) {
    while let Ok(evt) = channels.rx.try_recv() {
        match evt {
            InternalWandEvent::Connected(glasses, wand_id) => {
                let board_entity = board.get_single().ok();
                let entity = commands
                    .spawn((
                        SpatialBundle::default(),
                        Wand {
                            glasses: glasses.clone(),
                            id: wand_id,
                            hand: WandHand::Unknown,
                            battery: 0,
                        },
                        WandOwner(glasses.clone()),
                        WandAnalog::default(),
                        WandButtonsState::default(),
                    ))
                    .id();
                if let Some(board_entity) = board_entity {
                    commands.entity(board_entity).add_child(entity);
                }
                entities.0.insert((glasses.clone(), wand_id), entity);
                events.send(WandEvent::Connected { glasses, wand: wand_id });
            }
            InternalWandEvent::Disconnected(glasses, wand_id) => {
                if let Some(entity) = entities.0.remove(&(glasses.clone(), wand_id)) {
                    commands.entity(entity).despawn_recursive();
                }
                events.send(WandEvent::Disconnected { glasses, wand: wand_id });
            }
            InternalWandEvent::Desync(glasses, wand_id) => {
                events.send(WandEvent::Desync { glasses, wand: wand_id });
            }
            InternalWandEvent::Report(glasses, wand_id, report) => {
                if let Some(entity) = entities.0.get(&(glasses.clone(), wand_id)) {
                    if let Ok((mut t, mut analog, mut buttons, mut wand)) =
                        wand_q.get_mut(*entity)
                    {
                        if report.pose_valid {
                            let bevy_rot = rot_bevy_from_stage() * report.rot_stage_to_wand.conjugate();
                            let bevy_pos = rot_bevy_from_stage() * report.pos_grip_in_stage;
                            t.translation = bevy_pos;
                            t.rotation = bevy_rot;
                        }
                        if report.analog_valid {
                            analog.trigger = report.trigger;
                            analog.stick = report.stick;
                        }
                        if report.buttons_valid {
                            *buttons = report.buttons.into();
                        }
                        if report.battery_valid {
                            wand.battery = report.battery;
                        }
                        wand.hand = report.hand;
                    }
                }
                events.send(WandEvent::Report { glasses, wand: wand_id, report });
            }
        }
    }
}
