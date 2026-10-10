use super::tests::{battle, seconds};
use super::*;
use crate::arena::map::{ISLANDS, MAX_SCARS, PLATES, THRONE};

/// Looses an arrow that crosses `point` within a single step.
fn loose(arena: &mut Arena, point: Vec2, direction: Vec2, burning: f32) {
    arena.arrow = Arrow {
        state: ArrowState::Flying,
        position: point.minus(direction.scale(4.0)),
        velocity: direction.scale(8.0 / STEP),
        burning,
    };
    arena.tick_arrow(false);
}
fn standing() -> Controls {
    Controls::default()
}

#[test]
fn every_hit_on_the_carrier_splits_the_spark_until_its_heart_lies_bare() {
    let mut arena = battle(Guardian::Claude);
    for split in 1..=4u8 {
        for shard in arena.boss.shards.iter_mut().filter(|s| s.alive) {
            shard.position = CENTER.plus(Vec2::new(-60.0, 30.0));
            shard.airborne = false;
            shard.height = 0.0;
        }
        arena.boss.enter(BossState::Watching, 30.0);
        arena.boss.update_core();
        assert!(!arena.accepts_core_hit(arena.player.position));
        let center = arena.boss.body_center();
        loose(&mut arena, center, Vec2::new(1.0, 0.0), 0.0);
        assert_eq!(arena.boss.generation, split);
        assert_eq!(arena.phase, Phase::Battle);
        assert_eq!(arena.arrow.state, ArrowState::Lodged);
        assert_eq!(
            arena.boss.shards.iter().filter(|s| s.alive).count(),
            split as usize
        );
        // The struck carrier recoils into a new hop.
        assert_eq!(arena.boss.state, BossState::Striking);
    }
    assert!(arena.boss.exposed > 0.0);
    assert!(arena.boss.radius() < 4.0);
    for shard in &mut arena.boss.shards {
        shard.alive = false;
    }
    arena.boss.enter(BossState::Watching, 30.0);
    arena.boss.update_core();
    arena.boss.previous_core = arena.boss.core;
    let core = arena.boss.core;
    loose(&mut arena, core, Vec2::new(0.0, -1.0), 0.0);
    assert_eq!(arena.phase, Phase::Victory);
}

#[test]
fn a_split_spark_shatters_without_feeding_the_carrier() {
    let mut arena = battle(Guardian::Claude);
    arena.boss.enter(BossState::Watching, 30.0);
    let center = arena.boss.body_center();
    loose(&mut arena, center, Vec2::new(1.0, 0.0), 0.0);
    let ix = arena.boss.shards.iter().position(|s| s.alive).unwrap();
    let shard = &mut arena.boss.shards[ix];
    shard.position = arena.boss.position.plus(Vec2::new(30.0, 10.0));
    shard.airborne = false;
    shard.height = 0.0;
    let center = shard.center();
    loose(&mut arena, center, Vec2::new(0.0, -1.0), 0.0);
    assert!(!arena.boss.shards[ix].alive);
    assert_eq!(arena.boss.generation, 1);
    assert_eq!(arena.arrow.state, ArrowState::Lodged);
    assert!(arena.effects.iter().any(|e| e.kind == EffectKind::Burst));
}

#[test]
fn landing_sparks_leave_ember_slime_that_slows_but_never_kills() {
    let mut arena = battle(Guardian::Claude);
    arena.boss.enter(BossState::Watching, 30.0);
    arena.player.invulnerable = 0.0;
    let east = Controls {
        movement: Vec2::new(1.0, 0.0),
        ..Controls::default()
    };
    let start = arena.player.position;
    seconds(&mut arena, 0.1, east);
    let normal = arena.player.position.minus(start).length();
    let player = arena.player.position;
    arena.hazard(HazardKind::Goo, player, 0.0, 5.5, 6.0);
    seconds(&mut arena, 0.1, east);
    assert!(arena.player.slowed);
    assert!(arena.player.position.minus(player).length() < normal * 0.7);
    seconds(&mut arena, 1.0, standing());
    assert_eq!(arena.phase, Phase::Battle);
}

#[test]
fn a_leap_onto_a_forge_plate_lights_the_brazier_that_ignites_arrows() {
    let mut arena = battle(Guardian::Codex);
    arena.player.invulnerable = f32::INFINITY;
    assert!(!arena.brazier_lit());
    arena.boss.attack = Attack::Leap;
    arena.boss.from = PLATES[1].minus(Vec2::new(20.0, 0.0));
    arena.boss.position = arena.boss.from;
    arena.boss.target = PLATES[1].plus(Vec2::new(1.5, 0.5));
    arena.boss.enter(BossState::Striking, 0.85);
    for _ in 0..(1.0 / STEP) as usize {
        arena.tick_boss();
        if arena.brazier_lit() {
            break;
        }
    }
    assert_eq!(arena.boss.plate, Some(1));
    assert_eq!(arena.boss.position, PLATES[1]);
    // The keyboard aim offers the flame first.
    assert_eq!(arena.aim_target(), flame_center());
    arena.player.position = flame_center().plus(Vec2::new(0.0, 30.0));
    arena.arrow = Arrow {
        state: ArrowState::Flying,
        position: arena.player.position,
        velocity: Vec2::new(0.0, -130.0),
        burning: 0.0,
    };
    for _ in 0..(0.4 / STEP) as usize {
        arena.tick_arrow(false);
    }
    assert!(arena.arrow.burning > 0.0);
    // The plate rises once the knot moves on, and the flame dies with it.
    seconds(&mut arena, 3.3, standing());
    assert!(!arena.brazier_lit());
}

#[test]
fn only_a_burning_arrow_melts_the_knot_and_the_cube_opens_between_shield_pulses() {
    let mut arena = battle(Guardian::Codex);
    arena.boss.position = CENTER.plus(Vec2::new(0.0, -20.0));
    arena.boss.enter(BossState::Watching, 30.0);
    arena.boss.update_core();
    let center = arena.boss.body_center();
    loose(&mut arena, center, Vec2::new(0.0, -1.0), 0.0);
    assert_eq!(arena.boss.exposed, 0.0);
    assert_eq!(arena.arrow.state, ArrowState::Lodged);
    loose(&mut arena, center, Vec2::new(0.0, -1.0), 2.0);
    assert!(arena.boss.exposed > 7.0);
    assert_eq!(arena.arrow.state, ArrowState::Lodged);
    assert!(arena.boss.cube_shielded());
    arena.boss.update_core();
    arena.boss.previous_core = arena.boss.core;
    let core = arena.boss.core;
    loose(&mut arena, core, Vec2::new(0.0, -1.0), 0.0);
    assert_eq!(arena.phase, Phase::Battle);
    assert_eq!(arena.arrow.state, ArrowState::Lodged);
    arena.boss.shield = 0.9;
    assert!(!arena.boss.cube_shielded());
    loose(&mut arena, core, Vec2::new(0.0, -1.0), 0.0);
    assert_eq!(arena.phase, Phase::Victory);
}

#[test]
fn three_rolling_rushes_break_columns_and_end_in_a_leap() {
    let mut arena = battle(Guardian::Codex);
    arena.player.invulnerable = f32::INFINITY;
    let pillar = arena.map.pillars()[0];
    arena.boss.position = pillar.position.minus(Vec2::new(10.4, 0.0));
    arena.boss.direction = Vec2::new(1.0, 0.0);
    arena.boss.attack = Attack::Rush;
    arena.boss.dashes = 3;
    arena.boss.enter(BossState::Rushing, 2.5);
    arena.tick_boss();
    assert!(!arena.map.intact(0));
    assert_eq!(
        (arena.boss.state, arena.boss.attack, arena.boss.dashes),
        (BossState::Windup, Attack::Rush, 2)
    );
    assert_eq!(arena.boss.exposed, 0.0);
    for _ in 0..(8.0 / STEP) as usize {
        arena.tick_boss();
        if arena.boss.attack == Attack::Leap {
            break;
        }
    }
    assert_eq!(arena.boss.attack, Attack::Leap);
    assert_eq!(arena.boss.state, BossState::Windup);
}

#[test]
fn the_stem_on_the_archers_side_strikes_while_the_other_guards_the_eye() {
    let mut arena = battle(Guardian::Pi);
    arena.player.invulnerable = f32::INFINITY;
    arena.player.position = THRONE.plus(Vec2::new(-24.0, 30.0));
    seconds(&mut arena, 2.0, standing());
    assert_eq!(arena.boss.guard, 1);
    let mut struck = false;
    for _ in 0..(3.0 / STEP) as usize {
        arena.tick(standing());
        if arena.boss.state == BossState::Striking && arena.boss.attack == Attack::LeftFist {
            struck = true;
            break;
        }
    }
    assert!(struck);
    assert_eq!(arena.boss.exposed, 0.0);
    let (guard, _) = arena.boss.guard_point(1);
    assert!(arena.boss.hands[1].minus(guard).length() < 1.5);
    assert!(arena.boss.hands[0].minus(arena.player.position).length() < 8.0);
    // Crossing the midline forces a switch that leaves the eye bare.
    arena.player.position = THRONE.plus(Vec2::new(24.0, 30.0));
    let mut switched = false;
    for _ in 0..(1.5 / STEP) as usize {
        arena.tick(standing());
        if arena.boss.attack == Attack::Switch {
            switched = true;
            break;
        }
    }
    assert!(switched);
    assert_eq!(arena.boss.guard, 0);
    assert!(arena.boss.exposed > 1.0);
    // The incoming guard stays clear of the aperture for most of the window.
    seconds(&mut arena, 0.6, standing());
    assert!(arena.boss.hand_heights[0] > 10.0);
}

#[test]
fn a_guarding_stem_or_closed_glyph_stops_an_arrow_and_only_the_bare_eye_wins() {
    let mut arena = battle(Guardian::Pi);
    arena.player.invulnerable = f32::INFINITY;
    arena.player.position = THRONE.plus(Vec2::new(-24.0, 30.0));
    seconds(&mut arena, 2.0, standing());
    let core = arena.boss.core;
    arena.boss.exposed = 1.0;
    loose(&mut arena, core, Vec2::new(0.0, -1.0), 0.0);
    assert_eq!(arena.phase, Phase::Battle, "the guard stem covers the eye");
    assert_eq!(arena.arrow.state, ArrowState::Lodged);
    let guard = arena.boss.guard;
    arena.boss.hand_heights[guard] = 18.0;
    loose(&mut arena, core, Vec2::new(0.0, -1.0), 0.0);
    assert_eq!(arena.phase, Phase::Victory);
}

#[test]
fn the_stems_cannot_reach_the_south_gate_so_far_archers_are_stoned() {
    let mut arena = battle(Guardian::Pi);
    arena.player.invulnerable = f32::INFINITY;
    let gate = Vec2::new(THRONE.x - 1.0, 99.0);
    arena.player.position = gate;
    let mut barrage = false;
    for _ in 0..(12.0 / STEP) as usize {
        arena.tick(standing());
        let boss = arena.boss;
        if boss.state == BossState::Striking
            && matches!(boss.attack, Attack::LeftFist | Attack::RightFist)
        {
            // Clear of both the slam and the ring it throws out.
            let gap = boss.target.minus(gate).length();
            assert!(gap > 8.0 + PLAYER_RADIUS + 0.45, "{gap}");
        }
        barrage |= arena
            .projectiles
            .iter()
            .any(|p| p.kind == ProjectileKind::Block);
    }
    assert!(barrage);
}

#[test]
fn spat_blocks_land_as_obstacles_and_a_landing_crushes_them() {
    let mut arena = battle(Guardian::OpenCode);
    arena.player.invulnerable = f32::INFINITY;
    arena.boss.enter(BossState::Watching, 30.0);
    let spot = CENTER.plus(Vec2::new(-20.0, 25.0));
    arena.hazard(HazardKind::Drop, spot, 0.3, 0.12, 2.6);
    seconds(&mut arena, 0.2, standing());
    assert!(arena.map.blocks.is_empty());
    seconds(&mut arena, 0.2, standing());
    assert_eq!(arena.map.blocks, vec![spot]);
    let from = spot.minus(Vec2::new(8.0, 0.0));
    assert!(arena.map.walk(from, spot, PLAYER_RADIUS).x < spot.x - 3.0);
    arena.impact(spot.plus(Vec2::new(3.0, 0.0)), 7.0);
    assert!(arena.map.blocks.is_empty());
}

#[test]
fn the_vanished_chest_reappears_on_the_spot_the_archer_held_at_its_lock() {
    let mut arena = battle(Guardian::OpenCode);
    arena.player.invulnerable = f32::INFINITY;
    arena.player.position = CENTER.plus(Vec2::new(35.0, 0.0));
    arena.boss.attacks = 1;
    arena.boss.enter(BossState::Watching, 0.0);
    arena.tick(standing());
    assert_eq!(arena.boss.state, BossState::Submerged);
    let spot = CENTER.plus(Vec2::new(0.0, 25.0));
    arena.player.position = spot;
    seconds(&mut arena, 0.5, standing());
    assert!(arena.boss.airborne());
    seconds(&mut arena, 0.25, standing());
    assert!(arena.boss.target_locked());
    let locked = arena.boss.target;
    assert!(locked.minus(spot).length() < 1.0);
    arena.player.position = spot.plus(Vec2::new(14.0, 0.0));
    seconds(&mut arena, 0.45, standing());
    assert_eq!(arena.boss.position, locked);
    assert!(!arena.boss.airborne());
}

#[test]
fn the_gape_fires_block_fans_while_the_mouth_is_open() {
    let mut arena = battle(Guardian::OpenCode);
    arena.player.invulnerable = f32::INFINITY;
    arena.player.position = arena.boss.position.plus(Vec2::new(0.0, 28.0));
    arena.boss.attacks = 4;
    arena.boss.enter(BossState::Watching, 0.0);
    seconds(&mut arena, 0.5, standing());
    assert_eq!(arena.boss.attack, Attack::Gape);
    assert!(arena.boss.exposed > 1.0);
    assert!(arena.accepts_core_hit(arena.player.position));
    seconds(&mut arena, 0.7, standing());
    assert!(
        arena
            .projectiles
            .iter()
            .filter(|p| p.kind == ProjectileKind::Block)
            .count()
            >= 5
    );
}

#[test]
fn a_breach_shatters_floes_and_bares_the_tail_only_as_it_falls() {
    let mut arena = battle(Guardian::DeepSeek);
    arena.player.invulnerable = f32::INFINITY;
    arena.player.position = ISLANDS[2].0.plus(Vec2::new(0.0, -20.0));
    arena.boss.attack = Attack::Breach;
    arena.boss.position = ISLANDS[2].0.minus(Vec2::new(2.0, 0.0));
    arena.boss.target = arena.boss.position;
    arena.boss.enter(BossState::Submerged, 0.05);
    seconds(&mut arena, 0.1, standing());
    assert_eq!(arena.boss.state, BossState::Striking);
    assert_ne!(arena.map.sunken & (1 << 2), 0);
    assert!(arena.boss.target.minus(arena.player.position).length() < 8.0);
    seconds(&mut arena, 0.4, standing());
    assert_eq!(arena.boss.exposed, 0.0);
    seconds(&mut arena, 0.35, standing());
    assert!(arena.boss.exposed > 0.0);
    seconds(&mut arena, 0.6, standing());
    assert!(arena.boss.exposed > 0.0, "the tail stays up after landing");
}

#[test]
fn swimming_prevents_drawing_but_not_recall_and_the_discharge_restores_every_floe() {
    let mut arena = battle(Guardian::DeepSeek);
    arena.player.invulnerable = f32::INFINITY;
    arena.boss.enter(BossState::Watching, 30.0);
    arena.player.position = CENTER;
    seconds(
        &mut arena,
        0.3,
        Controls {
            shoot: true,
            ..Controls::default()
        },
    );
    assert!(arena.player.swimming);
    assert_eq!(arena.player.charge, 0.0);
    arena.roll(Vec2::new(1.0, 0.0));
    assert_eq!(arena.player.roll_remaining, 0.0);
    arena.arrow.state = ArrowState::Lodged;
    arena.arrow.position = CENTER.plus(Vec2::new(6.0, 0.0));
    seconds(
        &mut arena,
        0.5,
        Controls {
            recall: true,
            ..Controls::default()
        },
    );
    assert_eq!(arena.arrow.state, ArrowState::Ready);
    arena.map.sunken = 0b1111;
    arena.player.position = ISLANDS[0].0;
    arena.boss.attack = Attack::Discharge;
    arena.boss.enter(BossState::Submerged, 0.1);
    seconds(&mut arena, 0.2, standing());
    assert_eq!(arena.map.sunken, 0);
    assert!(arena.map.on_island(arena.player.position));
    seconds(&mut arena, 1.0, standing());
    assert!(arena.waves.iter().any(|w| w.kind == WaveKind::Water));
}

#[test]
fn an_orb_knocks_the_archer_back_and_a_wall_behind_them_is_fatal() {
    let mut arena = battle(Guardian::Copilot);
    arena.boss.enter(BossState::Watching, 30.0);
    arena.player.invulnerable = 0.0;
    arena.player.position = CENTER.plus(Vec2::new(0.0, 20.0));
    let start = arena.player.position;
    arena.projectile(
        start.minus(Vec2::new(4.0, 0.0)),
        Vec2::new(46.0, 0.0),
        ProjectileKind::Orb,
    );
    seconds(&mut arena, 0.15, standing());
    assert!(arena.player.knocked());
    seconds(&mut arena, 1.0, standing());
    assert_eq!(arena.phase, Phase::Battle);
    assert!(arena.player.position.x > start.x + 6.0);
    arena.player.position = Vec2::new(151.0, 78.0);
    let start = arena.player.position;
    arena.projectile(
        start.minus(Vec2::new(4.0, 0.0)),
        Vec2::new(46.0, 0.0),
        ProjectileKind::Orb,
    );
    seconds(&mut arena, 0.4, standing());
    assert_eq!(arena.phase, Phase::Defeat);
}

#[test]
fn a_spin_dash_into_the_wall_stuns_it_facing_the_wall_with_the_jet_bare() {
    let mut arena = battle(Guardian::Copilot);
    arena.player.invulnerable = f32::INFINITY;
    arena.player.position = CENTER.plus(Vec2::new(-30.0, 0.0));
    arena.boss.position = Vec2::new(120.0, 80.0);
    arena.boss.dashes = 1;
    arena.boss.direction = Vec2::new(0.0, 1.0);
    arena.boss.attack = Attack::Dash;
    arena.boss.enter(BossState::Rushing, 3.0);
    for _ in 0..(1.0 / STEP) as usize {
        arena.tick(standing());
        if arena.boss.state == BossState::Recovery {
            break;
        }
    }
    assert_eq!(arena.boss.state, BossState::Recovery);
    assert!(arena.boss.exposed > 1.0);
    assert!(arena.boss.direction.y > 0.9);
    let core = arena.boss.core;
    assert!(!arena.accepts_core_hit(core.plus(Vec2::new(0.0, 6.0))));
    assert!(arena.accepts_core_hit(core.minus(Vec2::new(0.0, 6.0))));
    let ice = arena
        .hazards
        .iter()
        .filter(|h| h.kind == HazardKind::Icicle)
        .count();
    assert!(ice >= 5);
    assert!(arena.hazards.iter().all(|h| h.time < h.delay));
}

#[test]
fn idle_attack_loops_never_solve_the_player_created_puzzles() {
    for guardian in [Guardian::Claude, Guardian::Codex] {
        let mut arena = battle(guardian);
        arena.player.invulnerable = f32::INFINITY;
        for _ in 0..(40.0 / STEP) as usize {
            arena.tick(standing());
            assert_eq!(arena.boss.exposed, 0.0, "{guardian:?}");
            assert_eq!(arena.boss.generation, 0, "{guardian:?}");
        }
    }
}

#[test]
fn a_fast_arrow_hits_cover_before_a_core_and_destroyed_cover_stays_destroyed() {
    let mut arena = battle(Guardian::Codex);
    let pillar = arena.map.pillars()[0];
    arena.boss.position = pillar.position.plus(Vec2::new(0.0, -20.0));
    arena.boss.enter(BossState::Watching, 30.0);
    arena.boss.exposed = 5.0;
    arena.boss.shield = 0.9;
    arena.boss.update_core();
    arena.boss.previous_core = arena.boss.core;
    let core = arena.boss.core;
    let start = Vec2::new(core.x, pillar.position.y + 12.0);
    arena.arrow = Arrow {
        state: ArrowState::Flying,
        position: start,
        velocity: Vec2::new(0.0, -6000.0),
        burning: 0.0,
    };
    arena.tick_arrow(false);
    assert_eq!(arena.phase, Phase::Battle);
    assert_eq!(arena.arrow.state, ArrowState::Lodged);
    arena.break_pillar(0);
    arena.arrow = Arrow {
        state: ArrowState::Flying,
        position: start,
        velocity: Vec2::new(0.0, -6000.0),
        burning: 0.0,
    };
    arena.tick_arrow(false);
    assert_eq!(arena.phase, Phase::Victory);
}

#[test]
fn input_buffers_bridge_short_recovery_and_cancel_with_focus_input() {
    let mut arena = battle(Guardian::Codex);
    arena.boss.enter(BossState::Watching, 30.0);
    arena.roll(Vec2::new(1.0, 0.0));
    seconds(&mut arena, 0.22, standing());
    arena.roll(Vec2::new(0.0, 1.0));
    seconds(&mut arena, 0.12, standing());
    assert!(arena.player.roll_remaining > 0.15);
    assert_eq!(arena.player.facing, Vec2::new(0.0, 1.0));
    seconds(&mut arena, 0.3, standing());
    arena.player.charge = MIN_CHARGE;
    arena.hit_pause = 0.05;
    arena.release_shot(Some(Vec2::new(30.0, 80.0)));
    assert_eq!(arena.arrow.state, ArrowState::Ready);
    seconds(&mut arena, 0.08, standing());
    assert_eq!(arena.arrow.state, ArrowState::Flying);
    arena.arrow = Arrow::default();
    arena.player.charge = MIN_CHARGE;
    arena.hit_pause = 0.05;
    arena.release_shot(None);
    arena.cancel_input();
    seconds(&mut arena, 0.08, standing());
    assert_eq!(arena.arrow.state, ArrowState::Ready);
}

#[test]
fn terrain_scars_are_bounded_and_retain_the_most_recent_impacts() {
    let mut map = Map::new(Guardian::Claude);
    for ix in 0..100 {
        map.scar(
            Vec2::new(
                (ix % 20) as f32 * 6.0 + 20.0,
                (ix / 20) as f32 * 10.0 + 30.0,
            ),
            4.0,
            ScarKind::Crack,
        );
    }
    assert_eq!(map.scars.len(), MAX_SCARS);
    assert_eq!(map.scars.last().unwrap().position, Vec2::new(134.0, 70.0));
}

#[test]
fn a_moving_core_uses_relative_sweep_instead_of_only_its_final_position() {
    let mut arena = battle(Guardian::DeepSeek);
    arena.boss.exposed = 2.0;
    let center = arena.boss.core;
    arena.boss.previous_core = center.minus(Vec2::new(8.0, 0.0));
    arena.boss.core = center.plus(Vec2::new(8.0, 0.0));
    arena.arrow = Arrow {
        state: ArrowState::Flying,
        position: center,
        velocity: Vec2::new(0.0, 1.0),
        burning: 0.0,
    };
    arena.tick_arrow(false);
    assert_eq!(arena.phase, Phase::Victory);
}

#[test]
fn fast_player_motion_cannot_tunnel_through_a_landing_icicle() {
    let mut arena = battle(Guardian::Copilot);
    arena.player.invulnerable = 0.0;
    arena.player.position = Vec2::new(84.0, 70.0);
    arena.hazard(HazardKind::Icicle, Vec2::new(80.0, 70.0), 0.0, 0.1, 3.0);
    arena.tick_threats(Vec2::new(76.0, 70.0));
    assert_eq!(arena.phase, Phase::Defeat);
}
