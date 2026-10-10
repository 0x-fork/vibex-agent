use super::*;
use crate::arena::{
    combat::{Attack, Controls, HazardKind},
    map::{CENTER, ISLANDS, Map, PLATES},
};

fn windup(guardian: Guardian, attack: Attack) -> Arena {
    let mut arena = Arena::new(guardian, 0.0);
    arena.phase = Phase::Battle;
    arena.boss.attack = attack;
    arena.boss.enter(BossState::Windup, 1.0);
    arena.boss.time = 0.99;
    arena
}

fn cues(arena: &Arena, reduced: bool) -> Raster {
    let mut frame = Raster::with_offset(WIDTH, HEIGHT, 0, TOP_PAD);
    effects::ground(&mut frame, arena, reduced);
    frame
}

fn marked(frame: &Raster, p: Vec2, inks: &[u8]) -> bool {
    let (x, y) = pixel(p);
    (-1..=1).any(|dy| (-1..=1).any(|dx| inks.contains(&frame.get(x + dx, y + dy))))
}

#[test]
fn a_spin_dash_warns_its_full_route_and_wall_impact_through_flight() {
    let mut arena = windup(Guardian::Copilot, Attack::Dash);
    arena.boss.from = Vec2::new(40.0, 45.0);
    arena.boss.position = arena.boss.from;
    arena.boss.target = Vec2::new(140.0, 45.0);
    arena.boss.direction = Vec2::new(1.0, 0.0);
    let windup = cues(&arena, false);
    arena.boss.enter(BossState::Rushing, 3.0);
    arena.boss.position = Vec2::new(90.0, 45.0);
    let flight = cues(&arena, false);
    for frame in [&windup, &flight] {
        let (x, y) = pixel(arena.boss.target);
        let r = (arena.boss.impact_radius() * SCALE).ceil() as i32;
        assert!((-2..=2).any(|d| frame.get(x + r + d, y) == WARN));
        assert!(marked(
            frame,
            Vec2::new(120.0, 45.0 - arena.boss.radius()),
            &[WARN]
        ));
    }
}

#[test]
fn hop_and_leap_landings_mark_the_exact_impact_ring_before_the_body_arrives() {
    for (guardian, attack) in [
        (Guardian::Claude, Attack::Hop),
        (Guardian::Codex, Attack::Leap),
        (Guardian::OpenCode, Attack::Leap),
    ] {
        let mut arena = windup(guardian, attack);
        arena.boss.target = CENTER.plus(Vec2::new(20.0, 10.0));
        let frame = cues(&arena, false);
        let (x, y) = pixel(arena.boss.target);
        let r = (arena.boss.impact_radius() * SCALE).ceil() as i32;
        assert!(
            (-1..=1).any(|d| frame.get(x + r + d, y) == WARN),
            "{guardian:?}"
        );
    }
}

#[test]
fn the_discharge_marks_every_dry_floe_as_shelter() {
    let mut arena = windup(Guardian::DeepSeek, Attack::Discharge);
    arena.boss.enter(BossState::Submerged, 1.0);
    arena.map.sunken = 0b0010;
    let frame = cues(&arena, false);
    for (ix, (position, radius)) in ISLANDS.iter().enumerate() {
        let edge = position.plus(Vec2::new(radius + 0.75, 0.0));
        assert_eq!(
            marked(&frame, edge, &[IVORY]),
            ix != 1,
            "floe {ix} shelter marking"
        );
    }
}

#[test]
fn a_pressed_forge_plate_is_drawn_lit_and_the_others_dark() {
    let mut arena = Arena::new(Guardian::Codex, 0.0);
    arena.phase = Phase::Battle;
    arena.boss.plate = Some(2);
    let frame = cues(&arena, false);
    for (ix, plate) in PLATES.iter().enumerate() {
        let r = crate::arena::map::PLATE_RADIUS;
        let edge = plate.plus(Vec2::new(r, 0.0));
        assert_eq!(marked(&frame, edge, &[FIRE]), ix == 2, "plate {ix}");
    }
}

#[test]
fn falling_blocks_and_icicles_grow_a_shadow_before_they_land() {
    let mut arena = Arena::new(Guardian::Copilot, 0.0);
    arena.phase = Phase::Battle;
    let spot = CENTER.plus(Vec2::new(10.0, 20.0));
    arena.hazard(HazardKind::Icicle, spot, 0.6, 0.1, 3.0);
    let early = cues(&arena, false);
    arena.hazards[0].time = 0.5;
    let late = cues(&arena, false);
    let shade = |frame: &Raster| frame.pixels.iter().filter(|c| **c == SHADOW).count();
    assert!(shade(&late) > shade(&early));
    assert!(marked(&late, spot.plus(Vec2::new(3.0, 0.0)), &[WARN]));
}

#[test]
fn submerged_floes_erase_walkable_art_without_changing_the_cached_floor() {
    let intact = Map::new(Guardian::DeepSeek);
    let mut map = intact.clone();
    map.sunken = 0b1111;
    let cached = floor(Guardian::DeepSeek);
    let mut overlay = Raster::with_offset(WIDTH, HEIGHT, 0, TOP_PAD);
    scenery::damage(&mut overlay, &map);
    let mut submerged = cached.clone();
    submerged.blit(&overlay, 0, 0);
    for (position, radius) in ISLANDS {
        let center = pixel(position);
        let r = (radius * SCALE).ceil() as i32;
        for y in center.1 - r..=center.1 + r {
            for x in center.0 - r..=center.0 + r {
                let ground = Vec2::new(x as f32 / SCALE, y as f32 / SCALE);
                if intact.on_island(ground) {
                    assert!(!map.on_island(ground));
                    assert!(
                        matches!(submerged.get(x, y), WATER_DARK | WATER | WATER_LIGHT),
                        "walkable art remains at {x}, {y}"
                    );
                }
            }
        }
    }
    map.sunken = 0;
    let mut restored = Raster::with_offset(WIDTH, HEIGHT, 0, TOP_PAD);
    scenery::damage(&mut restored, &map);
    assert!(restored.pixels.iter().all(|ink| *ink == CLEAR));
}

#[test]
fn a_hit_flashes_the_model_white_and_the_flash_fades() {
    let mut arena = Arena::new(Guardian::Codex, 0.0);
    arena.phase = Phase::Battle;
    arena.boss.flash = 0.12;
    let lit = battle(&arena, false);
    arena.boss.flash = 0.0;
    let plain = battle(&arena, false);
    let white = |frame: &Raster| frame.pixels.iter().filter(|c| **c == WHITE).count();
    assert!(white(&lit) > white(&plain) + 100);
    arena.tick(Controls::default());
}
