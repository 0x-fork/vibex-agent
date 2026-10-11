use super::*;

const DASH_SPEED: f32 = 70.0;

impl Arena {
    pub(super) fn tick_copilot(&mut self) {
        let t = self.boss.progress();
        match self.boss.state {
            BossState::Watching => {
                let hover = self.boss.hover_height(self.visual_time);
                self.boss.height += (hover - self.boss.height) * (STEP * 5.0).min(1.0);
                let toward = self.player.position.minus(self.boss.position);
                let strafe = toward.normalized().perpendicular().scale(STEP * 9.0);
                let range = (toward.length() - 30.0).clamp(-1.0, 1.0) * STEP * 8.0;
                self.boss.position = self.map.move_body(
                    self.boss.position,
                    self.boss
                        .position
                        .plus(strafe)
                        .plus(toward.normalized().scale(range)),
                    self.boss.radius(),
                );
                self.boss.face(self.player.position, STEP * 4.0);
                self.boss.bank += (0.18 - self.boss.bank) * (STEP * 3.0).min(1.0);
                self.boss.pitch *= 0.95;
                if self.boss.time >= self.boss.duration {
                    if self.boss.attacks.is_multiple_of(2) {
                        self.boss.begin(Attack::Volley, self.player.position, 0.35);
                    } else {
                        self.boss.dashes = 1 + (self.random() % 3) as u8;
                        self.boss.begin(Attack::Dash, self.player.position, 0.45);
                    }
                }
            }
            BossState::Windup if self.boss.attack == Attack::Volley => {
                self.boss.face(self.player.position, STEP * 6.0);
                if t >= 1.0 {
                    self.boss.enter(BossState::Striking, 1.0);
                }
            }
            BossState::Windup => {
                // Spin up low, lock the line, then hurl along it.
                if !self.boss.target_locked() {
                    self.boss.direction =
                        self.player.position.minus(self.boss.position).normalized();
                    self.boss.target = self.map.beam_end(self.boss.position, self.boss.direction);
                }
                let direction = self.boss.direction;
                self.boss.turn_to(direction.angle() - PI * 0.5, STEP * 8.0);
                self.boss.direction = direction;
                self.boss.height += (2.0 - self.boss.height) * (STEP * 8.0).min(1.0);
                self.boss.bank += STEP * 16.0 * t;
                if t >= 1.0 {
                    self.boss.from = self.boss.position;
                    self.boss.enter(BossState::Rushing, 3.0);
                }
            }
            BossState::Striking => {
                self.boss.face(self.player.position, STEP * 4.0);
                let throw = (self.boss.time / 0.32) as u32 + 1;
                if throw > self.boss.fired && throw <= 3 {
                    self.boss.fired = throw;
                    let origin = self.boss.attack_origin();
                    let aim = self.player.position.minus(origin).normalized();
                    self.projectile(origin, aim.scale(46.0), ProjectileKind::Orb);
                    self.effect(EffectKind::Spark, origin, 0.3, 3.0);
                }
                if t >= 1.0 {
                    self.boss.enter(BossState::Watching, 0.65);
                }
            }
            BossState::Rushing => {
                self.boss.bank += STEP * 22.0;
                if let Some(contact) = self.charge(DASH_SPEED) {
                    self.impact(contact, self.boss.impact_radius());
                    self.boss.dashes = self.boss.dashes.saturating_sub(1);
                    if self.boss.dashes > 0 {
                        self.boss.begin(Attack::Dash, self.player.position, 0.45);
                    } else {
                        self.stun();
                    }
                } else if t >= 1.0 {
                    self.stun();
                }
            }
            BossState::Recovery => {
                // Dazed against the wall: the rear jet sputters toward the room.
                let upright = (self.boss.bank / TAU).round() * TAU;
                self.boss.bank += (upright - self.boss.bank) * (STEP * 6.0).min(1.0);
                self.boss.height += (2.5 - self.boss.height) * (STEP * 4.0).min(1.0);
                if ((self.boss.time / 0.25) as u32) > self.boss.fired && self.boss.exposed > 0.0 {
                    self.boss.fired = (self.boss.time / 0.25) as u32;
                    self.effect(EffectKind::Steam, self.boss.core, 0.5, 4.0);
                }
                if t >= 1.0 {
                    self.boss.bank = (self.boss.bank + PI).rem_euclid(TAU) - PI;
                    self.boss.enter(BossState::Watching, 0.75);
                }
            }
            _ => {}
        }
    }

    fn stun(&mut self) {
        let direction = self.boss.direction;
        self.boss.turn_to(direction.angle() - PI * 0.5, TAU);
        self.boss.direction = direction;
        self.boss.update_core();
        self.boss.previous_core = self.boss.core;
        self.boss.exposed = 1.4;
        self.boss.enter(BossState::Recovery, 1.5);
        self.effect(EffectKind::Spark, self.boss.body_center(), 0.8, 8.0);
        // A row of shards shakes loose across the archer's line.
        let row = direction.perpendicular();
        for n in 0..9 {
            let spot = self.player.position.plus(row.scale((n as f32 - 4.0) * 8.0));
            if self.map.inside(spot, 1.5) {
                self.hazard(HazardKind::Icicle, spot, 0.55 + n as f32 * 0.05, 0.1, 3.0);
            }
        }
    }
}
