use super::*;
use crate::arena::map::ISLANDS;

impl Arena {
    pub(super) fn tick_deepseek(&mut self) {
        let t = self.boss.progress();
        match self.boss.state {
            BossState::Watching => {
                self.boss.face(self.player.position, STEP * 3.0);
                if self.boss.time >= self.boss.duration {
                    self.hunt();
                }
            }
            BossState::Submerged => {
                self.boss.height += (-4.0 - self.boss.height) * (STEP * 8.0).min(1.0);
                self.boss.pitch *= 0.9;
                let goal = if self.boss.attack == Attack::Discharge {
                    CENTER
                } else {
                    if !self.boss.target_locked() {
                        self.boss.target = self.map.move_body(
                            self.boss.target,
                            self.player.position,
                            self.boss.radius(),
                        );
                    }
                    // Surface a run-up short of the committed landing.
                    let back = self.boss.position.minus(self.boss.target);
                    let back = if back.length() > 1.0 {
                        back.normalized()
                    } else {
                        self.boss.direction.scale(-1.0)
                    };
                    self.map.move_body(
                        self.boss.target,
                        self.boss.target.plus(back.scale(18.0)),
                        self.boss.radius(),
                    )
                };
                let delta = goal.minus(self.boss.position);
                // After the lock it paces itself to surface exactly on time.
                let remaining = (self.boss.duration - self.boss.time).max(STEP);
                let speed = if self.boss.target_locked() {
                    (delta.length() / remaining).max(40.0)
                } else {
                    45.0
                };
                if delta.length() > 0.5 {
                    self.boss.position = self
                        .boss
                        .position
                        .plus(delta.normalized().scale((speed * STEP).min(delta.length())));
                    self.boss.turn_to(delta.angle() - PI * 0.5, STEP * 6.0);
                }
                if ((self.boss.time / 0.18) as u32) > self.boss.fired {
                    self.boss.fired = (self.boss.time / 0.18) as u32;
                    self.effect(EffectKind::Wake, self.boss.position, 0.8, 6.0);
                }
                if t >= 1.0 {
                    self.boss.position = goal;
                    if self.boss.attack == Attack::Discharge {
                        self.boss.enter(BossState::Striking, 2.2);
                        self.restore_floes();
                    } else {
                        self.breach();
                    }
                }
            }
            BossState::Striking if self.boss.attack == Attack::Discharge => {
                self.boss.height += (-2.0 - self.boss.height) * (STEP * 6.0).min(1.0);
                if ((self.boss.time / 0.12) as u32) > self.boss.fired && self.boss.time < 1.0 {
                    self.boss.fired = (self.boss.time / 0.12) as u32;
                    let angle = self.boss.fired as f32 * 2.3;
                    let spot = CENTER.plus(Vec2::new(angle.cos() * 40.0, angle.sin() * 26.0));
                    self.effect(EffectKind::Spark, spot, 0.5, 6.0);
                }
                if self.boss.time >= 1.0 && self.boss.fired < 100 {
                    self.boss.fired = 100;
                    self.wave(CENTER, 3.0, 110.0, WaveKind::Water);
                    self.effect(EffectKind::Spark, CENTER, 1.0, 16.0);
                }
                if t >= 1.0 {
                    self.hunt();
                }
            }
            BossState::Striking => {
                let t = self.boss.progress();
                self.boss.position = self.boss.from.lerp(self.boss.target, t);
                self.boss.height = (t * PI).sin() * 22.0 + self.boss.from_height * (1.0 - t);
                self.boss.pitch = (0.5 - t) * (t * PI).sin() * 1.2;
                if t > 0.55 {
                    self.boss.exposed = STEP * 2.0;
                }
                if t >= 1.0 {
                    self.boss.height = 0.0;
                    self.boss.pitch = 0.0;
                    self.splash(self.boss.position, self.boss.impact_radius());
                    self.boss.opening(0.7);
                }
            }
            BossState::Recovery if t >= 1.0 => {
                if self.boss.attacks.is_multiple_of(3) {
                    self.boss.attack = Attack::Discharge;
                    self.boss.from = self.boss.position;
                    self.boss.enter(BossState::Submerged, 1.0);
                } else {
                    self.hunt();
                }
            }
            _ => {}
        }
    }

    fn hunt(&mut self) {
        self.boss.attack = Attack::Breach;
        self.boss.attacks = self.boss.attacks.wrapping_add(1);
        self.boss.from = self.boss.position;
        self.boss.target = self.boss.position;
        self.boss.exposed = 0.0;
        self.boss.enter(BossState::Submerged, 1.1);
    }

    /// Erupts a run-up short of the committed spot and dives onto it, so
    /// the archer decides which floes the landing shatters.
    fn breach(&mut self) {
        let origin = self.boss.position;
        let direction = self.boss.target.minus(origin).normalized();
        self.boss.direction = direction;
        self.boss.turn_to(direction.angle() - PI * 0.5, TAU);
        self.boss.from = origin;
        self.boss.enter(BossState::Striking, 1.1);
        self.splash(origin, 5.0);
    }

    fn splash(&mut self, position: Vec2, radius: f32) {
        let broken = self.map.break_islands(position, radius);
        for (ix, (center, size)) in ISLANDS.iter().enumerate() {
            if broken & (1 << ix) != 0 {
                self.effect(EffectKind::Rubble, *center, 1.4, *size + 3.0);
            }
        }
        self.impact(position, radius);
        self.effect(EffectKind::Wake, position, 1.3, radius + 8.0);
    }

    fn restore_floes(&mut self) {
        let sunken = self.map.sunken;
        self.map.sunken = 0;
        for (ix, (position, radius)) in ISLANDS.iter().enumerate() {
            if sunken & (1 << ix) != 0 {
                self.effect(EffectKind::Wake, *position, 1.4, *radius + 3.0);
            }
        }
    }
}
