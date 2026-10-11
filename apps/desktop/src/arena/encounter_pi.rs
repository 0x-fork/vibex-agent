use super::*;
use crate::arena::map::{REACH, THRONE};

impl Arena {
    pub(super) fn tick_pi(&mut self) {
        let t = self.boss.progress();
        self.boss.position = THRONE;
        self.boss.previous_position = THRONE;
        self.boss.face(self.player.position, STEP * 0.8);
        self.boss.yaw = self.boss.yaw.clamp(-0.35, 0.35);
        let offset = self.player.position.x - THRONE.x;
        let active = 1 - self.boss.guard;
        // A small dead zone keeps the stems from flickering on the midline.
        let wanted = if offset < -4.0 {
            0
        } else if offset > 4.0 {
            1
        } else {
            active
        };
        let guard = self.boss.guard;
        let (guard_point, guard_height) = self.boss.guard_point(guard);
        // An incoming guard returns slowly and high, dropping in front of the
        // eye only once it arrives; until then the aperture is bare.
        let rate = if self.boss.attack == Attack::Switch && self.boss.state == BossState::Striking {
            2.2
        } else {
            10.0
        };
        let arriving = self.boss.hands[guard].minus(guard_point).length() > 5.0;
        let height = if arriving { 18.0 } else { guard_height };
        self.boss.hands[guard] = self.boss.hands[guard].lerp(guard_point, STEP * rate);
        self.boss.hand_heights[guard] +=
            (height - self.boss.hand_heights[guard]) * (STEP * rate * 1.5).min(1.0);
        let reach_point = self.reach_point();
        match self.boss.state {
            BossState::Watching => {
                self.hover(active, reach_point, 18.0, 8.0);
                if wanted != active {
                    // Trading stems leaves the aperture open for a moment.
                    self.boss.guard = active;
                    self.boss.attack = Attack::Switch;
                    self.boss.exposed = 1.3;
                    self.boss.enter(BossState::Striking, 1.4);
                    self.effect(EffectKind::Steam, self.boss.core, 0.8, 6.0);
                } else if self.boss.time >= self.boss.duration {
                    let far = self.player.position.minus(THRONE).length() > REACH;
                    if far && self.boss.attacks % 3 == 2 {
                        self.boss.begin(Attack::Barrage, self.player.position, 0.45);
                    } else {
                        let fist = if active == 0 {
                            Attack::LeftFist
                        } else {
                            Attack::RightFist
                        };
                        self.boss.begin(fist, reach_point, 0.42);
                    }
                }
            }
            BossState::Windup if self.boss.attack == Attack::Barrage => {
                self.hover(active, reach_point, 18.0, 6.0);
                if t >= 1.0 {
                    self.boss.enter(BossState::Striking, 0.9);
                }
            }
            BossState::Windup => {
                if !self.boss.target_locked() {
                    self.boss.target = reach_point;
                }
                self.hover(active, self.boss.target, 20.0, 14.0);
                if t >= 1.0 {
                    self.boss.from_height = self.boss.hand_heights[active];
                    self.boss.enter(BossState::Striking, 0.16);
                }
            }
            BossState::Striking if self.boss.attack == Attack::Switch => {
                let striker = 1 - self.boss.guard;
                self.hover(striker, reach_point, 18.0, 7.0);
                if t >= 1.0 {
                    self.boss.enter(BossState::Watching, 0.3);
                }
            }
            BossState::Striking if self.boss.attack == Attack::Barrage => {
                self.hover(active, reach_point, 18.0, 6.0);
                let volley = (t * 3.0) as u32 + 1;
                if volley > self.boss.fired && volley <= 3 {
                    self.boss.fired = volley;
                    let origin = self.boss.attack_origin();
                    self.projectile(
                        origin,
                        self.player.position.minus(origin).normalized().scale(40.0),
                        ProjectileKind::Block,
                    );
                    self.effect(EffectKind::Shot, origin, 0.3, 4.0);
                }
                if t >= 1.0 {
                    self.boss.enter(BossState::Recovery, 0.3);
                }
            }
            BossState::Striking => {
                let striker = 1 - self.boss.guard;
                self.boss.hands[striker] = self.boss.target;
                self.boss.hand_heights[striker] = self.boss.from_height * (1.0 - t * t);
                if t >= 1.0 {
                    let target = self.boss.target;
                    self.boss.hand_heights[striker] = 0.0;
                    self.impact(target, self.boss.impact_radius());
                    self.wave(target, 3.0, 8.0, WaveKind::Stone);
                    self.effect(EffectKind::Dust, target, 0.7, 8.0);
                    self.boss.enter(BossState::Recovery, 0.35);
                }
            }
            BossState::Recovery => {
                let striker = 1 - self.boss.guard;
                self.boss.hand_heights[striker] =
                    (self.boss.hand_heights[striker] + STEP * 30.0).min(6.0);
                if t >= 1.0 {
                    self.boss.enter(BossState::Watching, 0.3);
                }
            }
            _ => {}
        }
    }

    /// The archer's position clamped to the stems' reach and clear terrain.
    fn reach_point(&self) -> Vec2 {
        let delta = self.player.position.minus(THRONE);
        let goal = if delta.length() > REACH {
            THRONE.plus(delta.normalized().scale(REACH))
        } else {
            self.player.position
        };
        self.map.move_body(THRONE, goal, 2.0)
    }

    fn hover(&mut self, ix: usize, over: Vec2, height: f32, rate: f32) {
        let blend = (STEP * rate).min(1.0);
        self.boss.hands[ix] = self.boss.hands[ix].lerp(over, blend);
        self.boss.hand_heights[ix] += (height - self.boss.hand_heights[ix]) * blend;
    }
}
