//! Run the installed scout against Bumblebee without a window; no original executable.
use tf2_core::{
    character,
    combat::{Enemy, Scout},
    damage::{Packet, flags},
    sim::{self, Arena, State},
    tuning,
};
fn main() {
    let dir = std::path::Path::new("C:/Games2");
    let scout = Scout::load(dir).unwrap();
    let data = character::load(dir, "bumblebee").unwrap();
    let mut t = tuning::load(dir, "Bumblebee").unwrap();
    t.locomotion = tuning::Locomotion::from_animations(&data.animations, &mut t.missing);
    t.actions = tuning::Actions::from_animations(&data.animations, &data.robot, &mut t.missing);
    t.rule_sets = tuning::RuleSet::all(&data.animations);
    let mut player = State::new(&t);
    let mut enemy = Enemy::new(1, glam::Vec3::Y * 35.0, &scout);
    let mut rounds = 0;
    let mut hits = 0;
    for _ in 0..100 {
        for shot in enemy.step(
            &scout,
            &player,
            &t,
            &Arena::default(),
            false,
            sim::GAME_UPDATE,
        ) {
            rounds += 1;
            if shot.target == Some(0) {
                player.receive_damage(shot.packet, &t);
                hits += 1;
            }
        }
        sim::step(
            &mut player,
            &Default::default(),
            &t,
            &Arena::default(),
            sim::GAME_UPDATE,
        );
    }
    println!(
        "Scout HP {}, gun {} damage at {:.2}s interval; {rounds} rounds, {hits} hits, Bumblebee HP {:.1}",
        scout.tuning.base_health, scout.gun.damage, scout.gun.rate_of_fire, player.health
    );
    enemy.receive(
        Packet {
            source: Some(player.actor),
            flags: flags::STUN,
            stun_time: 2.5,
            ..Default::default()
        },
        &scout,
    );
    let fired = enemy.step(
        &scout,
        &player,
        &t,
        &Arena::default(),
        false,
        sim::GAME_UPDATE,
    );
    println!(
        "EMP: {:?}, {:.3}s stun remaining, {} rounds",
        enemy.state.control_state(&scout.tuning),
        enemy.state.damage_timers.stunned,
        fired.len()
    );
    enemy.receive(
        Packet {
            source: Some(player.actor),
            amount: 500.0,
            flags: flags::BY_EXPLOSION,
            ..Default::default()
        },
        &scout,
    );
    for _ in 0..340 {
        enemy.step(
            &scout,
            &player,
            &t,
            &Arena::default(),
            true,
            sim::GAME_UPDATE,
        );
    }
    println!(
        "Death: health {}, removed {}",
        enemy.state.health, enemy.state.restart_requested
    );
    assert!(hits > 0 && fired.is_empty() && enemy.state.restart_requested);
}
