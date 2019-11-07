use crate::runner::TestRunner;
use abstutil::Timer;
use geom::Duration;
use map_model::{BuildingID, IntersectionID};
use sim::{Event, Scenario, SidewalkSpot, SimFlags, TripEndpoint, TripSpec};

pub fn run(t: &mut TestRunner) {
    t.run_slow("bike_from_border", |h| {
        let mut flags = SimFlags::for_test("bike_from_border");
        flags.opts.savestate_every = Some(Duration::seconds(30.0));
        let (map, mut sim, mut rng) = flags.load(&mut Timer::throwaway());
        // TODO Hardcoding IDs is fragile
        let goal_bldg = BuildingID(319);
        let (ped, bike) = sim.schedule_trip(
            Duration::ZERO,
            TripSpec::UsingBike {
                start: SidewalkSpot::start_at_border(IntersectionID(186), &map).unwrap(),
                vehicle: Scenario::rand_bike(&mut rng),
                goal: TripEndpoint::Building(goal_bldg),
                ped_speed: Scenario::rand_ped_speed(&mut rng),
            },
            &map,
        );
        sim.spawn_all_trips(&map, &mut Timer::throwaway(), false);
        h.setup_done(&sim);

        sim.run_until_expectations_met(
            &map,
            vec![
                Event::BikeStoppedAtSidewalk(
                    bike.unwrap(),
                    map.get_b(goal_bldg).front_path.sidewalk.lane(),
                ),
                Event::PedReachedBuilding(ped.unwrap(), goal_bldg),
            ],
            Duration::minutes(7),
        );
        sim.just_run_until_done(&map, Some(Duration::minutes(1)));
    });
}
