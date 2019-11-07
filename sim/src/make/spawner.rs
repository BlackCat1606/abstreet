use crate::{
    CarID, Command, CreateCar, CreatePedestrian, ParkingSimState, ParkingSpot, PedestrianID,
    Scheduler, SidewalkPOI, SidewalkSpot, TripEndpoint, TripLeg, TripManager, VehicleSpec,
    VehicleType, MAX_CAR_LENGTH,
};
use abstutil::Timer;
use geom::{Duration, Speed, EPSILON_DIST};
use map_model::{BuildingID, BusRouteID, BusStopID, Map, PathRequest, Position};
use serde_derive::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum TripSpec {
    // Can be used to spawn from a border or anywhere for interactive debugging.
    CarAppearing {
        start_pos: Position,
        goal: TripEndpoint,
        vehicle_spec: VehicleSpec,
        ped_speed: Speed,
    },
    UsingParkedCar {
        start: SidewalkSpot,
        spot: ParkingSpot,
        goal: TripEndpoint,
        ped_speed: Speed,
    },
    MaybeUsingParkedCar {
        start_bldg: BuildingID,
        goal: TripEndpoint,
        ped_speed: Speed,
    },
    JustWalking {
        start: SidewalkSpot,
        goal: SidewalkSpot,
        ped_speed: Speed,
    },
    UsingBike {
        start: SidewalkSpot,
        goal: TripEndpoint,
        vehicle: VehicleSpec,
        ped_speed: Speed,
    },
    UsingTransit {
        start: SidewalkSpot,
        goal: SidewalkSpot,
        route: BusRouteID,
        stop1: BusStopID,
        stop2: BusStopID,
        ped_speed: Speed,
    },
}

#[derive(Serialize, Deserialize, PartialEq)]
pub struct TripSpawner {
    parked_cars_claimed: BTreeSet<CarID>,
    trips: Vec<(Duration, Option<PedestrianID>, Option<CarID>, TripSpec)>,
}

impl TripSpawner {
    pub fn new() -> TripSpawner {
        TripSpawner {
            parked_cars_claimed: BTreeSet::new(),
            trips: Vec::new(),
        }
    }

    pub fn schedule_trip(
        &mut self,
        start_time: Duration,
        ped_id: Option<PedestrianID>,
        car_id: Option<CarID>,
        spec: TripSpec,
        map: &Map,
        parking: &ParkingSimState,
    ) {
        // TODO We'll want to repeat this validation when we spawn stuff later for a second leg...
        match &spec {
            TripSpec::CarAppearing {
                start_pos,
                vehicle_spec,
                goal,
                ..
            } => {
                if start_pos.dist_along() < vehicle_spec.length {
                    panic!(
                        "Can't spawn a car at {}; too close to the start",
                        start_pos.dist_along()
                    );
                }
                if start_pos.dist_along() >= map.get_l(start_pos.lane()).length() {
                    panic!(
                        "Can't spawn a car at {}; {} isn't that long",
                        start_pos.dist_along(),
                        start_pos.lane()
                    );
                }
                match goal {
                    TripEndpoint::Lane(end_lane) => {
                        if start_pos.lane() == *end_lane
                            && start_pos.dist_along() == map.get_l(*end_lane).length()
                        {
                            panic!("Can't start a car at the edge of a border already");
                        }
                    }
                    TripEndpoint::Building(_) => {}
                }
            }
            TripSpec::UsingParkedCar { spot, .. } => {
                let car_id = parking.get_car_at_spot(*spot).unwrap().vehicle.id;
                if self.parked_cars_claimed.contains(&car_id) {
                    panic!(
                        "A TripSpec wants to use {}, which is already claimed",
                        car_id
                    );
                }
                self.parked_cars_claimed.insert(car_id);
            }
            TripSpec::MaybeUsingParkedCar { .. } => {}
            TripSpec::JustWalking { start, goal, .. } => {
                if start == goal {
                    panic!(
                        "A trip just walking from {:?} to {:?} doesn't make sense",
                        start, goal
                    );
                }
            }
            TripSpec::UsingBike { start, goal, .. } => {
                if SidewalkSpot::bike_from_bike_rack(start.sidewalk_pos.lane(), map).is_none() {
                    panic!(
                        "Can't start biking from {}; no biking or driving lane nearby?",
                        start.sidewalk_pos.lane()
                    );
                }
                if let TripEndpoint::Building(_) = goal {
                    let last_lane = goal.goal_pos_for_vehicle(map).lane();
                    // If bike_to_sidewalk works, then SidewalkSpot::bike_rack should too.
                    if map
                        .get_parent(last_lane)
                        .bike_to_sidewalk(last_lane)
                        .is_none()
                    {
                        panic!(
                            "Can't fulfill {:?} for a bike trip; no sidewalk near {}",
                            goal, last_lane
                        );
                    }
                }
            }
            TripSpec::UsingTransit { .. } => {}
        };

        self.trips.push((start_time, ped_id, car_id, spec));
    }

    pub fn spawn_all(
        &mut self,
        map: &Map,
        parking: &ParkingSimState,
        trips: &mut TripManager,
        scheduler: &mut Scheduler,
        timer: &mut Timer,
        retry_if_no_room: bool,
    ) {
        let paths = timer.parallelize(
            "calculate paths",
            std::mem::replace(&mut self.trips, Vec::new()),
            |tuple| {
                let req = tuple.3.get_pathfinding_request(map, parking);
                (tuple, req.clone(), map.pathfind(req))
            },
        );
        timer.start_iter("spawn trips", paths.len());
        for ((start_time, ped_id, car_id, spec), req, maybe_path) in paths {
            timer.next();
            if maybe_path.is_none() {
                timer.warn(format!("Some trip couldn't find the first path {}", req));
                continue;
            }
            let path = maybe_path.unwrap();
            match spec {
                TripSpec::CarAppearing {
                    start_pos,
                    vehicle_spec,
                    goal,
                    ped_speed,
                } => {
                    // Assumption: If a car is appearing at a border and driving to a building,
                    // then it's owned by that building. Otherwise we wind up with endless waves of
                    // parked cars that're never reused.
                    let owner = if let TripEndpoint::Building(b) = goal {
                        Some(b)
                    } else {
                        None
                    };
                    let vehicle = vehicle_spec.make(car_id.unwrap(), owner);
                    let mut legs = vec![TripLeg::Drive(vehicle.clone(), goal.clone())];
                    if let TripEndpoint::Building(b) = goal {
                        legs.push(TripLeg::Walk(
                            ped_id.unwrap(),
                            ped_speed,
                            SidewalkSpot::building(b, map),
                        ));
                    }
                    let trip =
                        trips.new_trip(start_time, TripEndpoint::Lane(start_pos.lane()), legs);
                    let router = goal.make_router_for_vehicle(path, map, vehicle.vehicle_type);
                    scheduler.quick_push(
                        start_time,
                        Command::SpawnCar(
                            CreateCar::for_appearing(vehicle, start_pos, router, trip),
                            retry_if_no_room,
                        ),
                    );
                }
                TripSpec::UsingParkedCar {
                    start,
                    spot,
                    goal,
                    ped_speed,
                } => {
                    let vehicle = &parking.get_car_at_spot(spot).unwrap().vehicle;
                    match start.connection {
                        SidewalkPOI::Building(b) => assert_eq!(vehicle.owner, Some(b)),
                        _ => unreachable!(),
                    };

                    let parking_spot = SidewalkSpot::parking_spot(spot, map, parking);

                    let mut legs = vec![
                        TripLeg::Walk(ped_id.unwrap(), ped_speed, parking_spot.clone()),
                        TripLeg::Drive(vehicle.clone(), goal.clone()),
                    ];
                    match goal {
                        TripEndpoint::Building(b) => {
                            legs.push(TripLeg::Walk(
                                ped_id.unwrap(),
                                ped_speed,
                                SidewalkSpot::building(b, map),
                            ));
                        }
                        TripEndpoint::Lane(_) => {}
                    }
                    let trip = trips.new_trip(
                        start_time,
                        TripEndpoint::Building(vehicle.owner.unwrap()),
                        legs,
                    );

                    scheduler.quick_push(
                        start_time,
                        Command::SpawnPed(CreatePedestrian {
                            id: ped_id.unwrap(),
                            speed: ped_speed,
                            start,
                            goal: parking_spot,
                            path,
                            trip,
                        }),
                    );
                }
                TripSpec::MaybeUsingParkedCar {
                    start_bldg,
                    goal,
                    ped_speed,
                } => {
                    let walk_to = SidewalkSpot::deferred_parking_spot(start_bldg, goal, map);
                    // Can't add TripLeg::Drive, because we don't know the vehicle yet! Plumb along
                    // the TripEndpoint, so we can expand the trip later.
                    let legs = vec![TripLeg::Walk(ped_id.unwrap(), ped_speed, walk_to.clone())];
                    let trip = trips.new_trip(start_time, TripEndpoint::Building(start_bldg), legs);

                    scheduler.quick_push(
                        start_time,
                        Command::SpawnPed(CreatePedestrian {
                            id: ped_id.unwrap(),
                            speed: ped_speed,
                            start: SidewalkSpot::building(start_bldg, map),
                            goal: walk_to,
                            // This is junk
                            path,
                            trip,
                        }),
                    );
                }
                TripSpec::JustWalking {
                    start,
                    goal,
                    ped_speed,
                } => {
                    let trip = trips.new_trip(
                        start_time,
                        match start.connection {
                            SidewalkPOI::Building(b) => TripEndpoint::Building(b),
                            SidewalkPOI::SuddenlyAppear => {
                                TripEndpoint::Lane(start.sidewalk_pos.lane())
                            }
                            SidewalkPOI::Border(_) => {
                                TripEndpoint::Lane(path.current_step().as_lane())
                            }
                            _ => unreachable!(),
                        },
                        vec![TripLeg::Walk(ped_id.unwrap(), ped_speed, goal.clone())],
                    );

                    scheduler.quick_push(
                        start_time,
                        Command::SpawnPed(CreatePedestrian {
                            id: ped_id.unwrap(),
                            speed: ped_speed,
                            start,
                            goal,
                            path,
                            trip,
                        }),
                    );
                }
                TripSpec::UsingBike {
                    start,
                    vehicle,
                    goal,
                    ped_speed,
                } => {
                    let walk_to =
                        SidewalkSpot::bike_from_bike_rack(start.sidewalk_pos.lane(), map).unwrap();
                    let mut legs = vec![
                        TripLeg::Walk(ped_id.unwrap(), ped_speed, walk_to.clone()),
                        TripLeg::Drive(vehicle.make(car_id.unwrap(), None), goal.clone()),
                    ];
                    match goal {
                        TripEndpoint::Building(b) => {
                            legs.push(TripLeg::Walk(
                                ped_id.unwrap(),
                                ped_speed,
                                SidewalkSpot::building(b, map),
                            ));
                        }
                        TripEndpoint::Lane(_) => {}
                    };
                    let trip = trips.new_trip(
                        start_time,
                        match start.connection {
                            SidewalkPOI::Building(b) => TripEndpoint::Building(b),
                            SidewalkPOI::SuddenlyAppear => {
                                TripEndpoint::Lane(start.sidewalk_pos.lane())
                            }
                            SidewalkPOI::Border(_) => {
                                TripEndpoint::Lane(path.current_step().as_lane())
                            }
                            _ => unreachable!(),
                        },
                        legs,
                    );

                    scheduler.quick_push(
                        start_time,
                        Command::SpawnPed(CreatePedestrian {
                            id: ped_id.unwrap(),
                            speed: ped_speed,
                            start,
                            goal: walk_to,
                            path,
                            trip,
                        }),
                    );
                }
                TripSpec::UsingTransit {
                    start,
                    route,
                    stop1,
                    stop2,
                    goal,
                    ped_speed,
                } => {
                    let walk_to = SidewalkSpot::bus_stop(stop1, map);
                    let trip = trips.new_trip(
                        start_time,
                        match start.connection {
                            SidewalkPOI::Building(b) => TripEndpoint::Building(b),
                            SidewalkPOI::SuddenlyAppear => {
                                TripEndpoint::Lane(start.sidewalk_pos.lane())
                            }
                            SidewalkPOI::Border(_) => {
                                TripEndpoint::Lane(path.current_step().as_lane())
                            }
                            _ => unreachable!(),
                        },
                        vec![
                            TripLeg::Walk(ped_id.unwrap(), ped_speed, walk_to.clone()),
                            TripLeg::RideBus(ped_id.unwrap(), route, stop2),
                            TripLeg::Walk(ped_id.unwrap(), ped_speed, goal),
                        ],
                    );

                    scheduler.quick_push(
                        start_time,
                        Command::SpawnPed(CreatePedestrian {
                            id: ped_id.unwrap(),
                            speed: ped_speed,
                            start,
                            goal: walk_to,
                            path,
                            trip,
                        }),
                    );
                }
            }
        }

        timer.start("finalize spawned trips");
        scheduler.finalize_batch();
        timer.stop("finalize spawned trips");
    }

    pub fn is_done(&self) -> bool {
        self.trips.is_empty()
    }
}

impl TripSpec {
    // If possible, fixes problems that schedule_trip would hit.
    pub fn spawn_car_at(pos: Position, map: &Map) -> Option<Position> {
        let len = map.get_l(pos.lane()).length();
        // There's no hope.
        if len <= MAX_CAR_LENGTH {
            return None;
        }

        if pos.dist_along() < MAX_CAR_LENGTH {
            Some(Position::new(pos.lane(), MAX_CAR_LENGTH))
        } else if pos.dist_along() == len {
            Some(Position::new(pos.lane(), pos.dist_along() - EPSILON_DIST))
        } else {
            Some(pos)
        }
    }

    fn get_pathfinding_request(&self, map: &Map, parking: &ParkingSimState) -> PathRequest {
        match self {
            TripSpec::CarAppearing {
                start_pos,
                vehicle_spec,
                goal,
                ..
            } => PathRequest {
                start: *start_pos,
                end: goal.goal_pos_for_vehicle(map),
                can_use_bus_lanes: vehicle_spec.vehicle_type == VehicleType::Bus,
                can_use_bike_lanes: vehicle_spec.vehicle_type == VehicleType::Bike,
            },
            TripSpec::UsingParkedCar { start, spot, .. } => PathRequest {
                start: start.sidewalk_pos,
                end: SidewalkSpot::parking_spot(*spot, map, parking).sidewalk_pos,
                can_use_bike_lanes: false,
                can_use_bus_lanes: false,
            },
            // Don't know where the parked car will be, so just make a dummy path that'll never
            // fail.
            TripSpec::MaybeUsingParkedCar { start_bldg, .. } => {
                let pos = map.get_b(*start_bldg).front_path.sidewalk;
                PathRequest {
                    start: pos,
                    end: pos,
                    can_use_bike_lanes: false,
                    can_use_bus_lanes: false,
                }
            }
            TripSpec::JustWalking { start, goal, .. } => PathRequest {
                start: start.sidewalk_pos,
                end: goal.sidewalk_pos,
                can_use_bike_lanes: false,
                can_use_bus_lanes: false,
            },
            TripSpec::UsingBike { start, .. } => PathRequest {
                start: start.sidewalk_pos,
                end: SidewalkSpot::bike_from_bike_rack(start.sidewalk_pos.lane(), map)
                    .unwrap()
                    .sidewalk_pos,
                can_use_bike_lanes: false,
                can_use_bus_lanes: false,
            },
            TripSpec::UsingTransit { start, stop1, .. } => PathRequest {
                start: start.sidewalk_pos,
                end: SidewalkSpot::bus_stop(*stop1, map).sidewalk_pos,
                can_use_bike_lanes: false,
                can_use_bus_lanes: false,
            },
        }
    }
}
