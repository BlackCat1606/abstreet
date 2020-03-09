use crate::{
    CarID, DrivingGoal, ParkingSpot, PersonID, SidewalkSpot, Sim, TripSpec, VehicleSpec,
    VehicleType, BIKE_LENGTH, MAX_CAR_LENGTH, MIN_CAR_LENGTH,
};
use abstutil::{fork_rng, Timer, WeightedUsizeChoice};
use geom::{Distance, Duration, Speed, Time};
use map_model::{
    BuildingID, BusRouteID, BusStopID, DirectedRoadID, FullNeighborhoodInfo, LaneID, Map,
    PathConstraints, Position, RoadID,
};
use rand::seq::SliceRandom;
use rand::Rng;
use rand_xorshift::XorShiftRng;
use serde_derive::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Scenario {
    pub scenario_name: String,
    pub map_name: String,

    // Higher-level ways of specifying stuff
    // None means seed all buses. Otherwise the route name must be present here.
    pub only_seed_buses: Option<BTreeSet<String>>,
    pub seed_parked_cars: Vec<SeedParkedCars>,
    pub spawn_over_time: Vec<SpawnOverTime>,
    pub border_spawn_over_time: Vec<BorderSpawnOverTime>,

    // Much more detailed
    pub population: Population,
}

// SpawnOverTime and BorderSpawnOverTime should be kept separate. Agents in SpawnOverTime pick
// their mode (use a car, walk, bus) based on the situation. When spawning directly a border,
// agents have to start as a car or pedestrian already.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct SpawnOverTime {
    pub num_agents: usize,
    // TODO use https://docs.rs/rand/0.5.5/rand/distributions/struct.Normal.html
    pub start_time: Time,
    pub stop_time: Time,
    pub start_from_neighborhood: String,
    pub goal: OriginDestination,
    pub percent_biking: f64,
    pub percent_use_transit: f64,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct BorderSpawnOverTime {
    pub num_peds: usize,
    pub num_cars: usize,
    pub num_bikes: usize,
    pub percent_use_transit: f64,
    // TODO use https://docs.rs/rand/0.5.5/rand/distributions/struct.Normal.html
    pub start_time: Time,
    pub stop_time: Time,
    pub start_from_border: DirectedRoadID,
    pub goal: OriginDestination,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct SeedParkedCars {
    pub neighborhood: String,
    pub cars_per_building: WeightedUsizeChoice,
}

impl Scenario {
    // TODO may need to fork the RNG a bit more
    pub fn instantiate(&self, sim: &mut Sim, map: &Map, rng: &mut XorShiftRng, timer: &mut Timer) {
        sim.set_name(self.scenario_name.clone());

        timer.start(format!("Instantiating {}", self.scenario_name));

        timer.start("load full neighborhood info");
        let neighborhoods = FullNeighborhoodInfo::load_all(map);
        timer.stop("load full neighborhood info");

        for s in &self.seed_parked_cars {
            if !neighborhoods.contains_key(&s.neighborhood) {
                panic!("Neighborhood {} isn't defined", s.neighborhood);
            }

            seed_parked_cars(
                sim,
                &s.cars_per_building,
                &neighborhoods[&s.neighborhood].buildings,
                &neighborhoods[&s.neighborhood].roads,
                rng,
                map,
                timer,
            );
        }

        // Don't let two pedestrians starting from one building use the same car.
        let mut reserved_cars: HashSet<CarID> = HashSet::new();

        for s in &self.spawn_over_time {
            if !neighborhoods.contains_key(&s.start_from_neighborhood) {
                panic!("Neighborhood {} isn't defined", s.start_from_neighborhood);
            }

            timer.start_iter("SpawnOverTime each agent", s.num_agents);
            for _ in 0..s.num_agents {
                timer.next();
                s.spawn_agent(rng, sim, &mut reserved_cars, &neighborhoods, map, timer);
            }
        }

        timer.start_iter("BorderSpawnOverTime", self.border_spawn_over_time.len());
        for s in &self.border_spawn_over_time {
            timer.next();
            s.spawn_peds(rng, sim, &neighborhoods, map, timer);
            s.spawn_cars(rng, sim, &neighborhoods, map, timer);
            s.spawn_bikes(rng, sim, &neighborhoods, map, timer);
        }

        let mut individ_parked_cars: Vec<(BuildingID, usize)> = Vec::new();
        for (b, cnt) in &self.population.individ_parked_cars {
            if *cnt != 0 {
                individ_parked_cars.push((*b, *cnt));
            }
        }
        individ_parked_cars.shuffle(rng);
        seed_individ_parked_cars(individ_parked_cars, sim, map, rng, timer);

        timer.start_iter("IndividTrip", self.population.individ_trips.len());
        for t in &self.population.individ_trips {
            timer.next();
            let spec = t.trip.clone().to_trip_spec(rng);
            sim.schedule_trip(t.depart, spec, map);
        }

        sim.spawn_all_trips(map, timer, true);

        // Do this AFTER spawn_all_trips, so the TripIDs don't clobber anything. What a hack. :(
        if let Some(ref routes) = self.only_seed_buses {
            for route in map.get_all_bus_routes() {
                if routes.contains(&route.name) {
                    sim.seed_bus_route(route, map, timer);
                }
            }
        } else {
            // All of them
            for route in map.get_all_bus_routes() {
                sim.seed_bus_route(route, map, timer);
            }
        }

        sim.seed_all_people(&self.population.people);
        timer.stop(format!("Instantiating {}", self.scenario_name));
    }

    pub fn save(&self) {
        abstutil::write_binary(
            abstutil::path_scenario(&self.map_name, &self.scenario_name),
            self,
        );
    }

    pub fn small_run(map: &Map) -> Scenario {
        let mut s = Scenario {
            scenario_name: "small_run".to_string(),
            only_seed_buses: None,
            map_name: map.get_name().to_string(),
            seed_parked_cars: vec![SeedParkedCars {
                neighborhood: "_everywhere_".to_string(),
                cars_per_building: WeightedUsizeChoice {
                    weights: vec![5, 5],
                },
            }],
            spawn_over_time: vec![SpawnOverTime {
                num_agents: 100,
                start_time: Time::START_OF_DAY,
                stop_time: Time::START_OF_DAY + Duration::seconds(5.0),
                start_from_neighborhood: "_everywhere_".to_string(),
                goal: OriginDestination::Neighborhood("_everywhere_".to_string()),
                percent_biking: 0.5,
                percent_use_transit: 0.5,
            }],
            // If there are no sidewalks/driving lanes at a border, scenario instantiation will
            // just warn and skip them.
            border_spawn_over_time: map
                .all_incoming_borders()
                .into_iter()
                .map(|i| BorderSpawnOverTime {
                    num_peds: 10,
                    num_cars: 10,
                    num_bikes: 10,
                    start_time: Time::START_OF_DAY,
                    stop_time: Time::START_OF_DAY + Duration::seconds(5.0),
                    start_from_border: i.some_outgoing_road(map),
                    goal: OriginDestination::Neighborhood("_everywhere_".to_string()),
                    percent_use_transit: 0.5,
                })
                .collect(),
            population: Population {
                people: Vec::new(),
                individ_trips: Vec::new(),
                individ_parked_cars: BTreeMap::new(),
            },
        };
        for i in map.all_outgoing_borders() {
            s.spawn_over_time.push(SpawnOverTime {
                num_agents: 10,
                start_time: Time::START_OF_DAY,
                stop_time: Time::START_OF_DAY + Duration::seconds(5.0),
                start_from_neighborhood: "_everywhere_".to_string(),
                goal: OriginDestination::EndOfRoad(i.some_incoming_road(map)),
                percent_biking: 0.5,
                percent_use_transit: 0.5,
            });
        }
        s
    }

    pub fn empty(map: &Map, name: &str) -> Scenario {
        Scenario {
            scenario_name: name.to_string(),
            map_name: map.get_name().to_string(),
            only_seed_buses: Some(BTreeSet::new()),
            seed_parked_cars: Vec::new(),
            spawn_over_time: Vec::new(),
            border_spawn_over_time: Vec::new(),
            population: Population {
                people: Vec::new(),
                individ_trips: Vec::new(),
                individ_parked_cars: BTreeMap::new(),
            },
        }
    }

    // No border agents here, because making the count work is hard.
    pub fn scaled_run(map: &Map, num_agents: usize) -> Scenario {
        Scenario {
            scenario_name: "scaled_run".to_string(),
            map_name: map.get_name().to_string(),
            only_seed_buses: Some(BTreeSet::new()),
            seed_parked_cars: vec![SeedParkedCars {
                neighborhood: "_everywhere_".to_string(),
                cars_per_building: WeightedUsizeChoice {
                    weights: vec![5, 5],
                },
            }],
            spawn_over_time: vec![SpawnOverTime {
                num_agents: num_agents,
                start_time: Time::START_OF_DAY,
                stop_time: Time::START_OF_DAY + Duration::seconds(5.0),
                start_from_neighborhood: "_everywhere_".to_string(),
                goal: OriginDestination::Neighborhood("_everywhere_".to_string()),
                percent_biking: 0.5,
                percent_use_transit: 0.5,
            }],
            border_spawn_over_time: Vec::new(),
            population: Population {
                people: Vec::new(),
                individ_trips: Vec::new(),
                individ_parked_cars: BTreeMap::new(),
            },
        }
    }

    pub fn rand_car(rng: &mut XorShiftRng) -> VehicleSpec {
        let length = Scenario::rand_dist(rng, MIN_CAR_LENGTH, MAX_CAR_LENGTH);
        VehicleSpec {
            vehicle_type: VehicleType::Car,
            length,
            max_speed: None,
        }
    }

    pub fn rand_bike(rng: &mut XorShiftRng) -> VehicleSpec {
        let max_speed = Some(Scenario::rand_speed(
            rng,
            Speed::miles_per_hour(8.0),
            Speed::miles_per_hour(10.0),
        ));
        VehicleSpec {
            vehicle_type: VehicleType::Bike,
            length: BIKE_LENGTH,
            max_speed,
        }
    }

    pub fn rand_dist(rng: &mut XorShiftRng, low: Distance, high: Distance) -> Distance {
        assert!(high > low);
        Distance::meters(rng.gen_range(low.inner_meters(), high.inner_meters()))
    }

    pub fn rand_speed(rng: &mut XorShiftRng, low: Speed, high: Speed) -> Speed {
        assert!(high > low);
        Speed::meters_per_second(rng.gen_range(
            low.inner_meters_per_second(),
            high.inner_meters_per_second(),
        ))
    }

    pub fn rand_ped_speed(rng: &mut XorShiftRng) -> Speed {
        // 2-3mph
        Scenario::rand_speed(
            rng,
            Speed::meters_per_second(0.894),
            Speed::meters_per_second(1.34),
        )
    }
}

impl SpawnOverTime {
    fn spawn_agent(
        &self,
        rng: &mut XorShiftRng,
        sim: &mut Sim,
        reserved_cars: &mut HashSet<CarID>,
        neighborhoods: &HashMap<String, FullNeighborhoodInfo>,
        map: &Map,
        timer: &mut Timer,
    ) {
        let spawn_time = rand_time(rng, self.start_time, self.stop_time);
        // Note that it's fine for agents to start/end at the same building. Later we might
        // want a better assignment of people per household, or workers per office building.
        let from_bldg = *neighborhoods[&self.start_from_neighborhood]
            .buildings
            .choose(rng)
            .unwrap();

        // What mode?
        if let Some(parked_car) = sim
            .get_parked_cars_by_owner(from_bldg)
            .into_iter()
            .find(|p| !reserved_cars.contains(&p.vehicle.id))
        {
            if let Some(goal) =
                self.goal
                    .pick_driving_goal(PathConstraints::Car, map, &neighborhoods, rng, timer)
            {
                reserved_cars.insert(parked_car.vehicle.id);
                let spot = parked_car.spot;
                sim.schedule_trip(
                    spawn_time,
                    TripSpec::UsingParkedCar {
                        start: SidewalkSpot::building(from_bldg, map),
                        spot,
                        goal,
                        ped_speed: Scenario::rand_ped_speed(rng),
                    },
                    map,
                );
                return;
            }
        }

        if rng.gen_bool(self.percent_biking) {
            if let Some(goal) =
                self.goal
                    .pick_driving_goal(PathConstraints::Bike, map, &neighborhoods, rng, timer)
            {
                let start_at = map.get_b(from_bldg).sidewalk();
                // TODO Just start biking on the other side of the street if the sidewalk
                // is on a one-way. Or at least warn.
                if map
                    .get_parent(start_at)
                    .sidewalk_to_bike(start_at)
                    .is_some()
                {
                    let ok = if let DrivingGoal::ParkNear(to_bldg) = goal {
                        let end_at = map.get_b(to_bldg).sidewalk();
                        map.get_parent(end_at).sidewalk_to_bike(end_at).is_some()
                            && start_at != end_at
                    } else {
                        true
                    };
                    if ok {
                        sim.schedule_trip(
                            spawn_time,
                            TripSpec::UsingBike {
                                start: SidewalkSpot::building(from_bldg, map),
                                vehicle: Scenario::rand_bike(rng),
                                goal,
                                ped_speed: Scenario::rand_ped_speed(rng),
                            },
                            map,
                        );
                        return;
                    }
                }
            }
        }

        if let Some(goal) = self.goal.pick_walking_goal(map, &neighborhoods, rng, timer) {
            let start_spot = SidewalkSpot::building(from_bldg, map);
            if start_spot == goal {
                timer.warn("Skipping walking trip between same two buildings".to_string());
                return;
            }

            if rng.gen_bool(self.percent_use_transit) {
                // TODO This throws away some work. It also sequentially does expensive
                // work right here.
                if let Some((stop1, stop2, route)) =
                    map.should_use_transit(start_spot.sidewalk_pos, goal.sidewalk_pos)
                {
                    sim.schedule_trip(
                        spawn_time,
                        TripSpec::UsingTransit {
                            start: start_spot,
                            route,
                            stop1,
                            stop2,
                            goal,
                            ped_speed: Scenario::rand_ped_speed(rng),
                        },
                        map,
                    );
                    return;
                }
            }

            sim.schedule_trip(
                spawn_time,
                TripSpec::JustWalking {
                    start: start_spot,
                    goal,
                    ped_speed: Scenario::rand_ped_speed(rng),
                },
                map,
            );
            return;
        }

        timer.warn(format!("Couldn't fulfill {:?} at all", self));
    }
}

impl BorderSpawnOverTime {
    fn spawn_peds(
        &self,
        rng: &mut XorShiftRng,
        sim: &mut Sim,
        neighborhoods: &HashMap<String, FullNeighborhoodInfo>,
        map: &Map,
        timer: &mut Timer,
    ) {
        if self.num_peds == 0 {
            return;
        }

        let start = if let Some(s) =
            SidewalkSpot::start_at_border(self.start_from_border.src_i(map), map)
        {
            s
        } else {
            timer.warn(format!(
                "Can't start_at_border for {} without sidewalk",
                self.start_from_border
            ));
            return;
        };

        for _ in 0..self.num_peds {
            let spawn_time = rand_time(rng, self.start_time, self.stop_time);
            if let Some(goal) = self.goal.pick_walking_goal(map, &neighborhoods, rng, timer) {
                if rng.gen_bool(self.percent_use_transit) {
                    // TODO This throws away some work. It also sequentially does expensive
                    // work right here.
                    if let Some((stop1, stop2, route)) =
                        map.should_use_transit(start.sidewalk_pos, goal.sidewalk_pos)
                    {
                        sim.schedule_trip(
                            spawn_time,
                            TripSpec::UsingTransit {
                                start: start.clone(),
                                route,
                                stop1,
                                stop2,
                                goal,
                                ped_speed: Scenario::rand_ped_speed(rng),
                            },
                            map,
                        );
                        continue;
                    }
                }

                sim.schedule_trip(
                    spawn_time,
                    TripSpec::JustWalking {
                        start: start.clone(),
                        goal,
                        ped_speed: Scenario::rand_ped_speed(rng),
                    },
                    map,
                );
            }
        }
    }

    fn spawn_cars(
        &self,
        rng: &mut XorShiftRng,
        sim: &mut Sim,
        neighborhoods: &HashMap<String, FullNeighborhoodInfo>,
        map: &Map,
        timer: &mut Timer,
    ) {
        if self.num_cars == 0 {
            return;
        }
        let lanes = pick_starting_lanes(
            self.start_from_border.lanes(PathConstraints::Car, map),
            false,
            map,
        );
        if lanes.is_empty() {
            timer.warn(format!(
                "Can't start {} cars at border for {}",
                self.num_cars, self.start_from_border
            ));
            return;
        };

        for _ in 0..self.num_cars {
            let spawn_time = rand_time(rng, self.start_time, self.stop_time);
            if let Some(goal) =
                self.goal
                    .pick_driving_goal(PathConstraints::Car, map, &neighborhoods, rng, timer)
            {
                let vehicle = Scenario::rand_car(rng);
                sim.schedule_trip(
                    spawn_time,
                    TripSpec::CarAppearing {
                        start_pos: Position::new(*lanes.choose(rng).unwrap(), vehicle.length),
                        vehicle_spec: vehicle,
                        goal,
                        ped_speed: Scenario::rand_ped_speed(rng),
                    },
                    map,
                );
            }
        }
    }

    fn spawn_bikes(
        &self,
        rng: &mut XorShiftRng,
        sim: &mut Sim,
        neighborhoods: &HashMap<String, FullNeighborhoodInfo>,
        map: &Map,
        timer: &mut Timer,
    ) {
        if self.num_bikes == 0 {
            return;
        }
        let lanes = pick_starting_lanes(
            self.start_from_border.lanes(PathConstraints::Bike, map),
            true,
            map,
        );
        if lanes.is_empty() {
            timer.warn(format!(
                "Can't start {} bikes at border for {}",
                self.num_bikes, self.start_from_border
            ));
            return;
        };

        for _ in 0..self.num_bikes {
            let spawn_time = rand_time(rng, self.start_time, self.stop_time);
            if let Some(goal) =
                self.goal
                    .pick_driving_goal(PathConstraints::Bike, map, &neighborhoods, rng, timer)
            {
                let bike = Scenario::rand_bike(rng);
                sim.schedule_trip(
                    spawn_time,
                    TripSpec::CarAppearing {
                        start_pos: Position::new(*lanes.choose(rng).unwrap(), bike.length),
                        vehicle_spec: bike,
                        goal,
                        ped_speed: Scenario::rand_ped_speed(rng),
                    },
                    map,
                );
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum OriginDestination {
    Neighborhood(String),
    EndOfRoad(DirectedRoadID),
    GotoBldg(BuildingID),
}

impl OriginDestination {
    fn pick_driving_goal(
        &self,
        constraints: PathConstraints,
        map: &Map,
        neighborhoods: &HashMap<String, FullNeighborhoodInfo>,
        rng: &mut XorShiftRng,
        timer: &mut Timer,
    ) -> Option<DrivingGoal> {
        match self {
            OriginDestination::Neighborhood(ref n) => Some(DrivingGoal::ParkNear(
                *neighborhoods[n].buildings.choose(rng).unwrap(),
            )),
            OriginDestination::GotoBldg(b) => Some(DrivingGoal::ParkNear(*b)),
            OriginDestination::EndOfRoad(dr) => {
                let goal = DrivingGoal::end_at_border(*dr, constraints, map);
                if goal.is_none() {
                    timer.warn(format!(
                        "Can't spawn a {:?} ending at border {}; no appropriate lanes there",
                        constraints, dr
                    ));
                }
                goal
            }
        }
    }

    fn pick_walking_goal(
        &self,
        map: &Map,
        neighborhoods: &HashMap<String, FullNeighborhoodInfo>,
        rng: &mut XorShiftRng,
        timer: &mut Timer,
    ) -> Option<SidewalkSpot> {
        match self {
            OriginDestination::Neighborhood(ref n) => Some(SidewalkSpot::building(
                *neighborhoods[n].buildings.choose(rng).unwrap(),
                map,
            )),
            OriginDestination::EndOfRoad(dr) => {
                let goal = SidewalkSpot::end_at_border(dr.dst_i(map), map);
                if goal.is_none() {
                    timer.warn(format!("Can't end_at_border for {} without a sidewalk", dr));
                }
                goal
            }
            OriginDestination::GotoBldg(b) => Some(SidewalkSpot::building(*b, map)),
        }
    }
}

fn seed_parked_cars(
    sim: &mut Sim,
    cars_per_building: &WeightedUsizeChoice,
    owner_buildings: &Vec<BuildingID>,
    neighborhoods_roads: &BTreeSet<RoadID>,
    base_rng: &mut XorShiftRng,
    map: &Map,
    timer: &mut Timer,
) {
    // Track the available parking spots per road, only for the roads in the appropriate
    // neighborhood.
    let mut total_spots = 0;
    let mut open_spots_per_road: BTreeMap<RoadID, Vec<ParkingSpot>> = BTreeMap::new();
    for id in neighborhoods_roads {
        let r = map.get_r(*id);
        let mut spots: Vec<ParkingSpot> = Vec::new();
        for (lane, _) in r
            .children_forwards
            .iter()
            .chain(r.children_backwards.iter())
        {
            spots.extend(sim.get_free_spots(*lane));
        }
        total_spots += spots.len();
        spots.shuffle(&mut fork_rng(base_rng));
        open_spots_per_road.insert(r.id, spots);
    }

    let mut new_cars = 0;
    let mut ok = true;
    timer.start_iter("seed parked cars for buildings", owner_buildings.len());
    for b in owner_buildings {
        timer.next();
        if !ok {
            continue;
        }
        for _ in 0..cars_per_building.sample(base_rng) {
            let mut forked_rng = fork_rng(base_rng);
            if let Some(spot) = find_spot_near_building(
                *b,
                &mut open_spots_per_road,
                neighborhoods_roads,
                map,
                timer,
            ) {
                sim.seed_parked_car(Scenario::rand_car(&mut forked_rng), spot, Some(*b));
                new_cars += 1;
            } else {
                // TODO This should be more critical, but neighborhoods can currently contain a
                // building, but not even its road, so this is inevitable.
                timer.warn(format!(
                    "No room to seed parked cars. {} total spots, {:?} of {} buildings requested, \
                     {} new cars so far. Searched from {}",
                    total_spots,
                    cars_per_building,
                    owner_buildings.len(),
                    new_cars,
                    b
                ));
                ok = false;
                break;
            }
        }
    }

    timer.note(format!(
        "Seeded {} of {} parking spots with cars, leaving {} buildings without cars",
        new_cars,
        total_spots,
        owner_buildings.len() - new_cars
    ));
}

fn seed_individ_parked_cars(
    individ_parked_cars: Vec<(BuildingID, usize)>,
    sim: &mut Sim,
    map: &Map,
    base_rng: &mut XorShiftRng,
    timer: &mut Timer,
) {
    let mut open_spots_per_road: BTreeMap<RoadID, Vec<ParkingSpot>> = BTreeMap::new();
    for spot in sim.get_all_parking_spots().1 {
        let r = match spot {
            ParkingSpot::Onstreet(l, _) => map.get_l(l).parent,
            ParkingSpot::Offstreet(b, _) => map.get_l(map.get_b(b).sidewalk()).parent,
        };
        open_spots_per_road
            .entry(r)
            .or_insert_with(Vec::new)
            .push(spot);
    }
    for spots in open_spots_per_road.values_mut() {
        spots.shuffle(base_rng);
    }
    let all_roads = map
        .all_roads()
        .iter()
        .map(|r| r.id)
        .collect::<BTreeSet<_>>();

    timer.start_iter("seed individual parked cars", individ_parked_cars.len());
    let mut ok = true;
    for (b, cnt) in individ_parked_cars {
        timer.next();
        if !ok {
            continue;
        }
        for _ in 0..cnt {
            // TODO Fork?
            if let Some(spot) =
                find_spot_near_building(b, &mut open_spots_per_road, &all_roads, map, timer)
            {
                sim.seed_parked_car(Scenario::rand_car(base_rng), spot, Some(b));
            } else {
                timer.warn("Not enough room to seed individual parked cars.".to_string());
                ok = false;
                break;
            }
        }
    }
}

// Pick a parking spot for this building. If the building's road has a free spot, use it. If not,
// start BFSing out from the road in a deterministic way until finding a nearby road with an open
// spot.
fn find_spot_near_building(
    b: BuildingID,
    open_spots_per_road: &mut BTreeMap<RoadID, Vec<ParkingSpot>>,
    neighborhoods_roads: &BTreeSet<RoadID>,
    map: &Map,
    timer: &mut Timer,
) -> Option<ParkingSpot> {
    let mut roads_queue: VecDeque<RoadID> = VecDeque::new();
    let mut visited: HashSet<RoadID> = HashSet::new();
    {
        let start = map.building_to_road(b).id;
        roads_queue.push_back(start);
        visited.insert(start);
    }

    loop {
        if roads_queue.is_empty() {
            timer.warn(format!(
                "Giving up looking for a free parking spot, searched {} roads of {}: {:?}",
                visited.len(),
                open_spots_per_road.len(),
                visited
            ));
        }
        let r = roads_queue.pop_front()?;
        if let Some(spots) = open_spots_per_road.get_mut(&r) {
            // TODO With some probability, skip this available spot and park farther away
            if !spots.is_empty() {
                return spots.pop();
            }
        }

        for next_r in map.get_next_roads(r).into_iter() {
            // Don't floodfill out of the neighborhood
            if !visited.contains(&next_r) && neighborhoods_roads.contains(&next_r) {
                roads_queue.push_back(next_r);
                visited.insert(next_r);
            }
        }
    }
}

fn rand_time(rng: &mut XorShiftRng, low: Time, high: Time) -> Time {
    assert!(high > low);
    Time::START_OF_DAY + Duration::seconds(rng.gen_range(low.inner_seconds(), high.inner_seconds()))
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct IndividTrip {
    pub person: PersonID,
    pub depart: Time,
    pub trip: SpawnTrip,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum SpawnTrip {
    CarAppearing {
        // TODO Replace start with building|border
        start: Position,
        goal: DrivingGoal,
        // For bikes starting at a border, use CarAppearing. UsingBike implies a walk->bike trip.
        is_bike: bool,
    },
    MaybeUsingParkedCar(BuildingID, DrivingGoal),
    UsingBike(SidewalkSpot, DrivingGoal),
    JustWalking(SidewalkSpot, SidewalkSpot),
    UsingTransit(SidewalkSpot, SidewalkSpot, BusRouteID, BusStopID, BusStopID),
}

impl SpawnTrip {
    pub fn to_trip_spec(self, rng: &mut XorShiftRng) -> TripSpec {
        match self {
            SpawnTrip::CarAppearing {
                start,
                goal,
                is_bike,
                ..
            } => TripSpec::CarAppearing {
                start_pos: start,
                goal,
                vehicle_spec: if is_bike {
                    Scenario::rand_bike(rng)
                } else {
                    Scenario::rand_car(rng)
                },
                ped_speed: Scenario::rand_ped_speed(rng),
            },
            SpawnTrip::MaybeUsingParkedCar(start_bldg, goal) => TripSpec::MaybeUsingParkedCar {
                start_bldg,
                goal,
                ped_speed: Scenario::rand_ped_speed(rng),
            },
            SpawnTrip::UsingBike(start, goal) => TripSpec::UsingBike {
                start,
                goal,
                vehicle: Scenario::rand_bike(rng),
                ped_speed: Scenario::rand_ped_speed(rng),
            },
            SpawnTrip::JustWalking(start, goal) => TripSpec::JustWalking {
                start,
                goal,
                ped_speed: Scenario::rand_ped_speed(rng),
            },
            SpawnTrip::UsingTransit(start, goal, route, stop1, stop2) => TripSpec::UsingTransit {
                start,
                goal,
                route,
                stop1,
                stop2,
                ped_speed: Scenario::rand_ped_speed(rng),
            },
        }
    }
}

fn pick_starting_lanes(mut lanes: Vec<LaneID>, is_bike: bool, map: &Map) -> Vec<LaneID> {
    let min_len = if is_bike { BIKE_LENGTH } else { MAX_CAR_LENGTH };
    lanes.retain(|l| map.get_l(*l).length() > min_len);

    if is_bike {
        // If there's a choice between bike lanes and otherwise, always use the bike lanes.
        let bike_lanes = lanes
            .iter()
            .filter(|l| map.get_l(**l).is_biking())
            .cloned()
            .collect::<Vec<LaneID>>();
        if !bike_lanes.is_empty() {
            lanes = bike_lanes;
        }
    }

    lanes
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Population {
    pub people: Vec<PersonSpec>,
    pub individ_trips: Vec<IndividTrip>,
    pub individ_parked_cars: BTreeMap<BuildingID, usize>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct PersonSpec {
    pub id: PersonID,
    pub home: Option<BuildingID>,
    // Index into individ_trips. Each trip is referenced exactly once; this representation doesn't
    // enforce that, but is less awkward than embedding trips here.
    pub trips: Vec<usize>,
}
