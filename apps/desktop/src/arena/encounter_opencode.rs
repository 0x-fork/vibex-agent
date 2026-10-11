use super::*;

const CYCLE: [Attack; 5] = [
    Attack::Spit,
    Attack::Ambush,
    Attack::Leap,
    Attack::Leap,
    Attack::Gape,
];

impl Arena {
    pub(super) fn tick_opencode(&mut self) {
        let t = self.boss.progress();
        match self.boss.state {
            BossState::Watching => {
                self.boss.height = 0.0;
                self.boss.fade = (self.boss.fade + STEP * 6.0).min(1.0);
                self.boss.lid = (self.boss.lid - STEP * 5.0).max(0.0);
                self.boss.face(self.player.position, STEP * 4.0);
                let near = self.player.position.minus(self.boss.position).length() < 18.0;
                if self.boss.time >= self.boss.duration && near {
                    // A reflex snap at anyone close does not advance the cycle.
                    let attacks = self.boss.attacks;
                    self.boss.begin(Attack::Bite, self.player.position, 0.25);
                    self.boss.attacks = attacks;
                } else if self.boss.time >= self.boss.duration {
                    let attack = CYCLE[self.boss.attacks as usize % CYCLE.len()];
                    if attack == Attack::Ambush {
                        self.boss.attack = Attack::Ambush;
                        self.boss.attacks = self.boss.attacks.wrapping_add(1);
                        self.boss.target = self.player.position;
                        self.boss.enter(BossState::Submerged, 1.1);
                    } else {
                        let windup = match attack {
                            Attack::Spit | Attack::Leap => 0.35,
                            _ => 0.4,
                        };
                        self.boss.begin(attack, self.player.position, windup);
                    }
                }
            }
            BossState::Windup => {
                match self.boss.attack {
                    Attack::Leap => {
                        self.follow_target(30.0);
                        self.boss.squash = smoothstep(t) * 0.6;
                    }
                    Attack::Bite => {
                        self.follow_target(12.0);
                        self.boss.lid = smoothstep(t);
                    }
                    Attack::Gape => self.boss.lid = smoothstep(t) * 0.7,
                    _ => self.boss.lid = smoothstep(t) * 0.8,
                }
                self.boss.face(self.player.position, STEP * 6.0);
                if t >= 1.0 {
                    self.boss.from = self.boss.position;
                    let (duration, exposed) = match self.boss.attack {
                        Attack::Spit => (1.4, 0.0),
                        Attack::Leap => (0.6, 0.0),
                        Attack::Gape => (1.6, 1.6),
                        _ => (0.2, 0.0),
                    };
                    self.boss.exposed = exposed;
                    self.boss.enter(BossState::Striking, duration);
                }
            }
            BossState::Striking => match self.boss.attack {
                Attack::Spit => {
                    self.boss.lid = 0.75 + (self.boss.time * 30.0).sin() * 0.15;
                    let volley = (self.boss.time / 0.2) as u32 + 1;
                    if volley > self.boss.fired && volley <= 6 {
                        self.boss.fired = volley;
                        let angle = volley as f32 * 2.4 + self.boss.attacks as f32;
                        let distance = 3.0 + (volley % 3) as f32 * 4.0;
                        let spot = self
                            .player
                            .position
                            .plus(Vec2::from_angle(angle).scale(distance));
                        let spot = self.map.move_body(self.player.position, spot, 2.6);
                        self.hazard(HazardKind::Drop, spot, 0.85, 0.12, 2.6);
                        self.effect(EffectKind::Shot, self.boss.attack_origin(), 0.3, 4.0);
                    }
                    if t >= 1.0 {
                        self.boss.enter(BossState::Watching, 0.35);
                    }
                }
                Attack::Leap => {
                    self.boss.lid = (t * PI).sin() * 0.3;
                    if self.arc(18.0) {
                        let position = self.boss.position;
                        self.impact(position, self.boss.impact_radius());
                        self.wave(position, 5.0, 26.0, WaveKind::Stone);
                        self.boss.enter(BossState::Watching, 0.3);
                    }
                }
                Attack::Gape => {
                    self.boss.lid = 1.0;
                    let fans = [0.06, 0.6];
                    if (self.boss.fired as usize) < fans.len()
                        && self.boss.time >= fans[self.boss.fired as usize]
                    {
                        self.boss.fired += 1;
                        let origin = self.boss.attack_origin();
                        let aim = self.player.position.minus(origin).angle();
                        for n in 0..5 {
                            let angle = aim + (n as f32 - 2.0) * 0.25;
                            self.projectile(
                                origin,
                                Vec2::from_angle(angle).scale(34.0),
                                ProjectileKind::Block,
                            );
                        }
                    }
                    if t >= 1.0 {
                        self.boss.exposed = 0.0;
                        self.boss.enter(BossState::Recovery, 0.3);
                    }
                }
                _ => {
                    // Bite, or the snap after reappearing beside the archer.
                    if self.boss.attack == Attack::Bite {
                        self.boss.position = self.boss.from.lerp(self.boss.target, smoothstep(t));
                    }
                    self.boss.lid = 1.0 - smoothstep(t);
                    if t >= 1.0 {
                        self.boss.lid = 0.0;
                        let reach = self.boss.impact_radius()
                            + if self.boss.attack == Attack::Ambush {
                                0.5
                            } else {
                                0.0
                            };
                        let point = self.boss.position.plus(self.boss.direction.scale(3.0));
                        self.impact(point, reach);
                        self.boss.enter(BossState::Recovery, 0.45);
                    }
                }
            },
            BossState::Submerged => {
                // Vanish, follow the archer unseen, then reappear jaws first.
                self.boss.fade = 1.0 - smoothstep(self.boss.time / 0.35);
                if !self.boss.target_locked() {
                    self.boss.target = self.map.move_body(
                        self.boss.target,
                        self.player.position,
                        self.boss.radius(),
                    );
                }
                if t >= 1.0 {
                    self.boss.position = self.boss.target;
                    self.boss.previous_position = self.boss.target;
                    self.boss.face(self.player.position, TAU);
                    self.boss.fade = 1.0;
                    self.boss.lid = 1.0;
                    self.boss.from = self.boss.position;
                    self.effect(EffectKind::Dust, self.boss.position, 0.6, 9.0);
                    self.boss.enter(BossState::Striking, 0.14);
                }
            }
            BossState::Recovery => {
                self.boss.lid = (self.boss.lid - STEP * 4.0).max(0.0);
                if t >= 1.0 {
                    self.boss.enter(BossState::Watching, 0.4);
                }
            }
            _ => {}
        }
    }
}
