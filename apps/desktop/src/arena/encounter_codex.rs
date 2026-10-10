use super::*;
use crate::arena::map::{PLATE_RADIUS, PLATES};

const RUSH_SPEED: f32 = 62.0;

impl Arena {
    pub(super) fn tick_codex(&mut self) {
        let t = self.boss.progress();
        if self.boss.exposed > 0.0 {
            self.boss.shield += STEP;
            self.boss.unravel = (self.boss.unravel + STEP * 4.0).min(1.0);
            if self.boss.exposed <= STEP {
                self.effect(EffectKind::Burst, self.boss.core, 0.8, 12.0);
            }
        } else {
            self.boss.unravel = (self.boss.unravel - STEP * 2.5).max(0.0);
            self.boss.shield = 0.0;
        }
        match self.boss.state {
            BossState::Watching => {
                self.boss.height = 0.0;
                self.boss.face(self.player.position, STEP * 4.0);
                self.boss.spin += STEP * 0.6;
                if self.boss.time >= self.boss.duration {
                    if self.boss.exposed > 0.0 {
                        self.boss.begin(Attack::Leap, self.player.position, 0.5);
                    } else {
                        self.boss.dashes = 3;
                        self.boss.begin(Attack::Rush, self.player.position, 0.5);
                    }
                }
            }
            BossState::Windup if self.boss.attack == Attack::Rush => {
                // The knot turns onto its rim, revs, then rolls at the archer.
                if !self.boss.target_locked() {
                    self.boss.direction =
                        self.player.position.minus(self.boss.position).normalized();
                    self.boss.target = self.boss.position.plus(self.boss.direction.scale(40.0));
                }
                self.rotate_yaw(self.boss.direction.angle(), STEP * 9.0);
                self.boss.spin += STEP * 14.0 * t;
                if t >= 1.0 {
                    self.boss.from = self.boss.position;
                    self.boss.enter(BossState::Rushing, 2.5);
                }
            }
            BossState::Windup => {
                self.follow_target(46.0);
                self.boss.face(self.player.position, STEP * 5.0);
                self.boss.squash = smoothstep(t) * 0.5;
                if t >= 1.0 {
                    self.boss.from = self.boss.position;
                    let flight = if self.boss.exposed > 0.0 { 0.7 } else { 0.85 };
                    self.boss.enter(BossState::Striking, flight);
                }
            }
            BossState::Rushing => {
                self.boss.spin += RUSH_SPEED * STEP / 9.0;
                if let Some(contact) = self.charge(RUSH_SPEED) {
                    self.impact(contact, 6.0);
                    self.boss.dashes = self.boss.dashes.saturating_sub(1);
                    if self.boss.dashes > 0 {
                        self.boss.begin(Attack::Rush, self.player.position, 0.42);
                    } else {
                        self.boss.begin(Attack::Leap, self.player.position, 0.55);
                    }
                } else if t >= 1.0 {
                    self.boss.begin(Attack::Leap, self.player.position, 0.55);
                }
            }
            BossState::Striking => {
                self.boss.spin += STEP * 3.0;
                let peak = if self.boss.exposed > 0.0 { 18.0 } else { 28.0 };
                if self.arc(peak) {
                    let position = self.boss.position;
                    self.impact(position, self.boss.impact_radius());
                    self.wave(position, 6.0, 30.0, WaveKind::Stone);
                    let plate = PLATES
                        .iter()
                        .position(|p| p.minus(position).length() < PLATE_RADIUS + 1.0);
                    match plate {
                        Some(ix) if self.boss.exposed <= 0.0 => {
                            self.boss.position = PLATES[ix];
                            self.boss.plate = Some(ix);
                            self.effect(EffectKind::Ignite, flame_center(), 0.9, 7.0);
                            self.boss.enter(BossState::Recovery, 3.0);
                        }
                        _ if self.boss.exposed > 0.0 => {
                            self.boss.enter(BossState::Watching, 0.35);
                        }
                        _ => self.boss.enter(BossState::Recovery, 0.5),
                    }
                }
            }
            BossState::Recovery => {
                self.boss.face(self.player.position, STEP * 2.0);
                if t >= 1.0 {
                    self.boss.plate = None;
                    self.boss.enter(BossState::Watching, 0.45);
                }
            }
            _ => {}
        }
    }

    /// A burning arrow melts the knot open; the bare cube keeps leaping.
    pub(in crate::arena::combat) fn unravel_knot(&mut self) {
        self.boss.exposed = 7.5;
        self.boss.shield = 0.0;
        self.boss.plate = None;
        self.boss.update_core();
        self.boss.previous_core = self.boss.core;
        self.boss.enter(BossState::Watching, 0.4);
        self.effect(EffectKind::Burst, self.boss.core, 1.0, 14.0);
        self.effect(EffectKind::Ignite, self.boss.core, 0.8, 8.0);
        self.hit_pause = 0.08;
        self.boss.flash = 0.12;
    }
}
