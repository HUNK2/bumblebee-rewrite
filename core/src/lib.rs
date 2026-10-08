//! The parts of the Transformers: Revenge of the Fallen work that do not depend on an engine:
//! readers for the game's file formats, a character's tuning, the movement rules, the cameras,
//! the sound mixer and the weapon rules. Used by the Bevy game (`game\`). Everything is read
//! from the user's install at runtime; nothing from the game is bundled.

pub mod aim;
pub mod animblend;
pub mod animrules;
pub mod camera;
pub mod character;
pub mod chatter;
pub mod climb;
pub mod control;
pub mod damage;
pub mod combat;
pub mod driving;
// The readers decode more of each format than either user needs so far.
#[allow(dead_code)]
pub mod formats;
pub mod fxstate;
pub mod footik;
pub mod gun;
pub mod hud;
pub mod lockon;
pub mod melee;
pub mod particles;
pub mod pose;
pub mod reticle;
pub mod ribbon;
pub mod rumble;
pub mod shake;
pub mod sim;
pub mod skidmark;
pub mod sound;
pub mod stepfx;
pub mod speedblur;
pub mod ssd;
pub mod tuning;
pub mod terrain;
pub mod texture_upscale;
pub mod vehicle;
pub mod collisionfx;
pub mod weapons;
