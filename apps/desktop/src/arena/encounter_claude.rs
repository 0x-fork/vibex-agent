use super::*;

const REST: [f32; 5] = [0.42, 0.38, 0.32, 0.27, 0.24];
const FLIGHT: [f32; 5] = [0.62, 0.56, 0.5, 0.46, 0.42];

impl Arena {
    pub(super) fn tick_claude(&mut self) {
        let generation = self.boss.generation.min(4) as usize;
        if generation >= 4 {
            // Four splits leave the heart bare for good.
            self.boss.exposed = 1.0;
        }
        let t = self.boss.progress();
        match self.boss.state {
            BossState::Watching => {
                self.boss.height = 0.0;
                self.boss.spin += STEP * 1.5;
                self.boss.face(self.player.position, STEP * 4.0);
                if self.boss.time >= self.boss.duration {
                    self.boss.begin(Attack::Hop, self.player.position, 0.24);
                }
            }
            BossState::Windup => {
                self.follow_target(30.0 + 2.0 * generation as f32);
                self.boss.squash = smoothstep(t) * 0.6;
                self.boss.spin += STEP * 4.0;
                if t >= 1.0 {
                    self.boss.from = self.boss.position;
                    self.boss.enter(BossState::Striking, FLIGHT[generation]);
                }
            }
            BossState::Striking => {
                self.boss.spin += STEP * 10.0;
                if self.arc(16.0 - generation as f32 * 1.5) {
                    let position = self.boss.position;
                    let scale = self.boss.scale();
                    self.impact(position, self.boss.impact_radius());
                    self.hazard(HazardKind::Goo, position, 0.0, 5.5, 2.0 + 4.5 * scale);
                    self.effect(EffectKind::Dust, position, 0.6, 6.0 * scale + 2.0);
                    self.boss.enter(BossState::Watching, REST[generation]);
                }
            }
            _ => {}
        }
        self.tick_shards();
    }

    /// Split sparks hop on their own rhythm straight at the archer.
    fn tick_shards(&mut self) {
        for ix in 0..self.boss.shards.len() {
            let mut shard = self.boss.shards[ix];
            if !shard.alive {
                continue;
            }
            shard.previous = shard.position;
            shard.time += STEP;
            shard.spin += STEP * if shard.airborne { 10.0 } else { 1.5 };
            let mut landed = false;
            if shard.airborne {
                let t = (shard.time / shard.duration).min(1.0);
                shard.position = shard.from.lerp(shard.target, t);
                shard.height = (t * PI).sin() * 10.0;
                if t >= 1.0 {
                    shard.airborne = false;
                    shard.height = 0.0;
                    shard.time = 0.0;
                    shard.duration = 0.6 + ix as f32 * 0.15;
                    landed = true;
                }
            } else if shard.time >= shard.duration {
                let delta = self.player.position.minus(shard.position);
                let goal = if delta.length() > 24.0 {
                    shard.position.plus(delta.normalized().scale(24.0))
                } else {
                    self.player.position
                };
                shard.from = shard.position;
                shard.target = self.map.move_body(shard.position, goal, shard.radius());
                shard.airborne = true;
                shard.time = 0.0;
                shard.duration = 0.55;
            }
            self.boss.shards[ix] = shard;
            if landed {
                self.impact_with(shard.position, 2.0 + 4.0 * shard.size, 0.0);
            }
        }
    }

    /// The arrow splits the carrier: a heartless spark bursts away on one
    /// side while the smaller heart-bearer recoils to the other.
    pub(in crate::arena::combat) fn split_spark(&mut self, direction: Vec2) {
        let side = direction.perpendicular();
        let origin = self.boss.position;
        let center = self.boss.body_center();
        self.boss.generation = (self.boss.generation + 1).min(4);
        let size = self.boss.scale();
        if let Some(slot) = self.boss.shards.iter().position(|s| !s.alive) {
            let target = self
                .map
                .move_body(origin, origin.plus(side.scale(16.0)), 7.5 * size);
            self.boss.shards[slot] = Shard {
                alive: true,
                position: origin,
                previous: origin,
                from: origin,
                target,
                height: 0.0,
                time: 0.0,
                duration: 0.42,
                airborne: true,
                size,
                spin: self.boss.spin,
            };
        }
        self.boss.attack = Attack::Hop;
        self.boss.from = origin;
        self.boss.target =
            self.map
                .move_body(origin, origin.minus(side.scale(16.0)), self.boss.radius());
        self.boss.enter(BossState::Striking, 0.42);
        self.effect(EffectKind::Burst, center, 0.8, 10.0 * size + 3.0);
        self.hit_pause = 0.07;
        self.boss.flash = 0.12;
        if self.boss.generation >= 4 {
            self.boss.exposed = 1.0;
            self.effect(EffectKind::Steam, center, 1.0, 6.0);
        }
    }
}
