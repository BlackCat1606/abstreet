use crate::plugins::sim::des_model::interval::{Delta, Interval};
use geom::{Acceleration, Distance, Duration, Speed};
use map_model::{Lane, Traversable};
use sim::{CarID, CarState, DrawCarInput, VehicleType};

const FOLLOWING_DISTANCE: Distance = Distance::const_meters(1.0);

pub struct Car {
    pub id: CarID,
    // Hack used for different colors
    pub state: CarState,
    pub car_length: Distance,
    pub max_accel: Acceleration,
    pub max_deaccel: Acceleration,

    pub start_dist: Distance,
    pub start_time: Duration,

    pub intervals: Vec<Interval>,
}

impl Car {
    // None if they're not on the lane by then. Also returns the interval index for debugging.
    pub fn dist_at(&self, t: Duration) -> Option<(Distance, usize)> {
        // TODO Binary search
        for (idx, i) in self.intervals.iter().enumerate() {
            if i.covers(t) {
                return Some((i.dist(t), idx));
            }
        }
        None
    }

    pub fn last_state(&self) -> (Distance, Speed, Duration) {
        if let Some(i) = self.intervals.last() {
            (i.end_dist, i.end_speed, i.end_time)
        } else {
            (self.start_dist, Speed::ZERO, self.start_time)
        }
    }

    pub fn get_stop_from_speed(&self, from_speed: Speed) -> Delta {
        // v_f = v_0 + a(t)
        let time_needed = -from_speed / self.max_deaccel;

        // d = (v_0)(t) + (1/2)(a)(t^2)
        let dist_covered = from_speed * time_needed
            + Distance::meters(
                0.5 * self.max_deaccel.inner_meters_per_second_squared()
                    * time_needed.inner_seconds().powi(2),
            );

        Delta::new(time_needed, dist_covered)
    }

    // Returns interval indices too.
    fn find_earliest_hit(&self, other: &Car) -> Option<(Duration, Distance, usize, usize)> {
        // TODO Do we ever have to worry about having the same intervals? I think this should
        // always find the earliest hit.
        // TODO A good unit test... Make sure find_hit is symmetric
        for (idx1, i1) in self.intervals.iter().enumerate() {
            for (idx2, i2) in other.intervals.iter().enumerate() {
                if let Some((time, dist)) = i1.intersection(i2) {
                    return Some((time, dist, idx1, idx2));
                }
            }
        }
        None
    }

    pub fn validate(&self) {
        assert!(!self.intervals.is_empty());
        assert!(self.intervals[0].start_dist >= self.car_length);

        for pair in self.intervals.windows(2) {
            assert_eq!(pair[0].end_time, pair[1].start_time);
            assert_eq!(pair[0].end_dist, pair[1].start_dist);
            assert_eq!(pair[0].end_speed, pair[1].start_speed);
        }

        for i in &self.intervals {
            let accel = (i.end_speed - i.start_speed) / (i.end_time - i.start_time);
            if accel >= Acceleration::ZERO && accel > self.max_accel {
                println!(
                    "{} accelerates {}, but can only do {}",
                    self.id, accel, self.max_accel
                );
            }
            if accel < Acceleration::ZERO && accel < self.max_deaccel {
                println!(
                    "{} decelerates {}, but can only do {}",
                    self.id, accel, self.max_deaccel
                );
            }
        }
    }

    pub fn get_draw_car(&self, front: Distance, lane: &Lane) -> DrawCarInput {
        DrawCarInput {
            id: self.id,
            waiting_for_turn: None,
            stopping_trace: None,
            state: self.state,
            vehicle_type: VehicleType::Car,
            on: Traversable::Lane(lane.id),
            body: lane
                .lane_center_pts
                .slice(front - self.car_length, front)
                .unwrap()
                .0,
        }
    }

    pub fn dump_intervals(&self) {
        for i in &self.intervals {
            println!(
                "- {}->{} during {}->{} ({}->{})",
                i.start_dist, i.end_dist, i.start_time, i.end_time, i.start_speed, i.end_speed
            );
        }
    }
}

impl Car {
    fn next_state(&mut self, dist_covered: Distance, final_speed: Speed, time_needed: Duration) {
        let (dist1, speed1, time1) = self.last_state();
        self.intervals.push(Interval::new(
            dist1,
            dist1 + dist_covered,
            time1,
            time1 + time_needed,
            speed1,
            final_speed,
        ));
    }

    pub fn accel_from_rest_to_speed_limit(&mut self, speed: Speed) {
        assert_eq!(self.last_state().1, Speed::ZERO);

        // v_f = v_0 + a(t)
        let time_needed = speed / self.max_accel;

        // d = (v_0)(t) + (1/2)(a)(t^2)
        // TODO Woops, don't have Duration^2
        let dist_covered = Distance::meters(
            0.5 * self.max_accel.inner_meters_per_second_squared()
                * time_needed.inner_seconds().powi(2),
        );

        self.next_state(dist_covered, speed, time_needed);
    }

    pub fn freeflow(&mut self, time: Duration) {
        let speed = self.last_state().1;
        // Should explicitly wait for some time
        assert_ne!(speed, Speed::ZERO);

        self.next_state(speed * time, speed, time);
    }

    pub fn freeflow_to_cross(&mut self, dist: Distance) {
        let speed = self.last_state().1;
        assert_ne!(dist, Distance::ZERO);

        self.next_state(dist, speed, dist / speed);
    }

    pub fn deaccel_to_rest(&mut self) {
        let speed = self.last_state().1;
        assert_ne!(speed, Speed::ZERO);

        let delta = self.get_stop_from_speed(speed);
        self.next_state(delta.dist, Speed::ZERO, delta.time);
    }

    pub fn maybe_follow(&mut self, leader: &mut Car) {
        let (hit_time, hit_dist, idx1, idx2) = match self.find_earliest_hit(leader) {
            Some(hit) => hit,
            None => {
                return;
            }
        };
        println!(
            "Collision at {}, {}. follower interval {}, leader interval {}",
            hit_time, hit_dist, idx1, idx2
        );

        let dist_behind = leader.car_length + FOLLOWING_DISTANCE;

        self.intervals.split_off(idx1 + 1);

        // Option 1: Might be too sharp.
        if true {
            {
                let mut our_adjusted_last = self.intervals.pop().unwrap();
                our_adjusted_last.end_speed = our_adjusted_last.speed(hit_time);
                our_adjusted_last.end_time = hit_time;
                our_adjusted_last.end_dist = hit_dist - dist_behind;
                self.intervals.push(our_adjusted_last);
            }

            {
                let them = &leader.intervals[idx2];
                self.intervals.push(Interval::new(
                    hit_dist - dist_behind,
                    them.end_dist - dist_behind,
                    hit_time,
                    them.end_time,
                    self.intervals.last().as_ref().unwrap().end_speed,
                    them.end_speed,
                ));
            }
        } else {
            // TODO This still causes impossible deaccel
            let them = &leader.intervals[idx2];
            let mut our_adjusted_last = self.intervals.pop().unwrap();
            our_adjusted_last.end_speed = them.end_speed;
            our_adjusted_last.end_time = them.end_time;
            our_adjusted_last.end_dist = them.end_dist - dist_behind;
            self.intervals.push(our_adjusted_last);
        }

        // TODO What if we can't manage the same accel/deaccel/speeds?
        for i in &leader.intervals[idx2 + 1..] {
            self.intervals.push(Interval::new(
                i.start_dist - dist_behind,
                i.end_dist - dist_behind,
                i.start_time,
                i.end_time,
                i.start_speed,
                i.end_speed,
            ));
        }
    }

    pub fn stop_at_end_of_lane(&mut self, lane: &Lane, speed_limit: Speed) {
        // TODO Argh, this code is awkward.
        // TODO Handle shorter lanes.
        self.accel_from_rest_to_speed_limit(speed_limit);
        let stopping_dist = self.get_stop_from_speed(speed_limit).dist;
        self.freeflow_to_cross(
            lane.length() - self.intervals.last().as_ref().unwrap().end_dist - stopping_dist,
        );
        self.deaccel_to_rest();
    }

    pub fn wait(&mut self, time: Duration) {
        let speed = self.last_state().1;
        assert_eq!(speed, Speed::ZERO);
        self.next_state(Distance::ZERO, Speed::ZERO, time);
    }
}