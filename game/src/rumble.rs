//! The pad's rumble: the game's own effect (`tf2_core::rumble`), stepped in its sixtieths
//! of a second and handed to the pads through Bevy. Not felt by anyone yet: captures have
//! no pad.

use std::time::Duration;

use bevy::input::gamepad::{GamepadRumbleIntensity, GamepadRumbleRequest};
use bevy::prelude::*;
use tf2_core::rumble::{Pad, UPDATE};

use crate::player::Player;

/// How long each update's strengths are asked for: until the next update replaces them,
/// with room for a late frame.
const HOLD: f32 = 0.1;

#[derive(Resource, Default)]
pub struct PadRumble {
    pad: Pad,
    /// Time not yet given to the effect.
    time: f32,
}

/// Starts the rumbles the movement asked for this frame and plays the effect on every pad.
/// The original's heavy motor is Bevy's strong one, its light motor the weak one.
pub fn play(
    time: Res<Time>,
    player: Single<&Player>,
    pads: Query<Entity, With<Gamepad>>,
    mut rumble: ResMut<PadRumble>,
    mut requests: MessageWriter<GamepadRumbleRequest>,
) {
    let rumble = &mut *rumble;
    for start in &player.state.rumbles {
        rumble.pad.start(start.kind, start.duration, start.strength);
    }
    rumble.time += time.delta_secs();
    let mut latest = None;
    while rumble.time >= UPDATE {
        rumble.time -= UPDATE;
        latest = rumble.pad.step().or(latest);
    }
    let Some([strong_motor, weak_motor]) = latest else { return };
    for gamepad in &pads {
        // Bevy adds rumbles up, so the one before is taken off first.
        requests.write(GamepadRumbleRequest::Stop { gamepad });
        if strong_motor > 0.0 || weak_motor > 0.0 {
            requests.write(GamepadRumbleRequest::Add {
                duration: Duration::from_secs_f32(HOLD),
                intensity: GamepadRumbleIntensity { strong_motor, weak_motor },
                gamepad,
            });
        }
    }
}
