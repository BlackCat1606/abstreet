use crate::{
    AgentID, CarID, Command, CreateCar, CreatePedestrian, DrivingGoal, Event, ParkingSimState,
    ParkingSpot, PedestrianID, PersonID, PersonSpec, Scheduler, SidewalkPOI, SidewalkSpot,
    TransitSimState, TripID, TripPhaseType, Vehicle, VehicleType, WalkingSimState,
};
use abstutil::{deserialize_btreemap, serialize_btreemap, Counter};
use geom::{Speed, Time};
use map_model::{
    BuildingID, BusRouteID, BusStopID, IntersectionID, Map, PathConstraints, PathRequest, Position,
};
use serde_derive::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct TripManager {
    trips: Vec<Trip>,
    people: Vec<Person>,
    // For quick lookup of active agents
    #[serde(
        serialize_with = "serialize_btreemap",
        deserialize_with = "deserialize_btreemap"
    )]
    active_trip_mode: BTreeMap<AgentID, TripID>,
    num_bus_trips: usize,
    unfinished_trips: usize,

    events: Vec<Event>,
}

impl TripManager {
    pub fn new() -> TripManager {
        TripManager {
            trips: Vec::new(),
            people: Vec::new(),
            active_trip_mode: BTreeMap::new(),
            num_bus_trips: 0,
            unfinished_trips: 0,
            events: Vec::new(),
        }
    }

    pub fn new_trip(&mut self, spawned_at: Time, start: TripStart, legs: Vec<TripLeg>) -> TripID {
        assert!(!legs.is_empty());
        // TODO Make sure the legs constitute a valid state machine.

        let id = TripID(self.trips.len());
        let mut mode = TripMode::Walk;
        for l in &legs {
            match l {
                TripLeg::Walk(_, _, ref spot) => {
                    if let SidewalkPOI::DeferredParkingSpot(_, _) = spot.connection {
                        mode = TripMode::Drive;
                    }
                }
                TripLeg::Drive(ref vehicle, _) => {
                    mode = TripMode::Drive;
                    if vehicle.vehicle_type == VehicleType::Bike {
                        mode = TripMode::Bike;
                    }
                }
                TripLeg::RideBus(_, _, _) => {
                    mode = TripMode::Transit;
                }
                TripLeg::ServeBusRoute(_, _) => {
                    // Confusing, because Transit usually means riding transit
                    mode = TripMode::Transit;
                }
            }
        }
        let end = match legs.last() {
            Some(TripLeg::Walk(_, _, ref spot)) => match spot.connection {
                SidewalkPOI::Building(b) => TripEnd::Bldg(b),
                SidewalkPOI::Border(i) => TripEnd::Border(i),
                SidewalkPOI::DeferredParkingSpot(_, ref goal) => match goal {
                    DrivingGoal::ParkNear(b) => TripEnd::Bldg(*b),
                    DrivingGoal::Border(i, _) => TripEnd::Border(*i),
                },
                _ => unreachable!(),
            },
            Some(TripLeg::Drive(_, ref goal)) => match goal {
                DrivingGoal::ParkNear(b) => TripEnd::Bldg(*b),
                DrivingGoal::Border(i, _) => TripEnd::Border(*i),
            },
            Some(TripLeg::ServeBusRoute(_, route)) => TripEnd::ServeBusRoute(*route),
            _ => unreachable!(),
        };
        let trip = Trip {
            id,
            spawned_at,
            finished_at: None,
            aborted: false,
            mode,
            legs: VecDeque::from(legs),
            start,
            end,
            person: None,
        };
        if !trip.is_bus_trip() {
            self.unfinished_trips += 1;
        }
        self.trips.push(trip);
        id
    }

    // Must be called after all of the new_trip()s.
    pub fn new_person(&mut self, spec: &PersonSpec) {
        assert_eq!(spec.id.0, self.people.len());
        for t in &spec.trips {
            let trip = &mut self.trips[*t];
            assert!(trip.person.is_none());
            trip.person = Some(spec.id);
        }

        let trip = &self.trips[spec.trips[0]];
        let person = Person {
            id: spec.id,
            // TODO Put TripID in the scenario.
            trips: spec.trips.iter().map(|t| TripID(*t)).collect(),
            // TODO aborted case is kind of a hack, because of initialization order
            state: if trip.aborted {
                PersonState::Limbo
            } else {
                match trip.start {
                    TripStart::Bldg(b) => PersonState::Inside(b),
                    TripStart::Border(_) => PersonState::OffMap,
                }
            },
        };
        self.people.push(person);
    }
    pub fn check_trips_have_people(&self) {
        let mut bus_trips = 0;
        let mut have_ppl = 0;
        let mut missing_ppl = 0;
        for t in &self.trips {
            if t.is_bus_trip() {
                bus_trips += 1;
                assert!(t.person.is_none());
            } else {
                if t.person.is_some() {
                    have_ppl += 1;
                } else {
                    missing_ppl += 1;
                }
            }
        }
        if missing_ppl > 0 {
            panic!(
                "{} bus trips, {} trips have a person, but {} trips do not!",
                bus_trips, have_ppl, missing_ppl
            );
        }
    }

    pub fn dynamically_override_legs(&mut self, id: TripID, legs: Vec<TripLeg>) {
        let trip = &mut self.trips[id.0];
        trip.legs = VecDeque::from(legs);
        // This is only for peds using a previously unknown parked car
        trip.mode = TripMode::Drive;
    }

    pub fn agent_starting_trip_leg(&mut self, agent: AgentID, t: TripID) {
        assert!(!self.active_trip_mode.contains_key(&agent));
        // TODO ensure a trip only has one active agent (aka, not walking and driving at the same
        // time)
        self.active_trip_mode.insert(agent, t);
        let trip = &self.trips[t.0];
        if trip.is_bus_trip() {
            self.num_bus_trips += 1;
        } else {
            self.people[trip.person.unwrap().0].state = PersonState::Trip(t);
        }
    }

    pub fn car_reached_parking_spot(
        &mut self,
        now: Time,
        car: CarID,
        spot: ParkingSpot,
        map: &Map,
        parking: &ParkingSimState,
        scheduler: &mut Scheduler,
    ) {
        self.events.push(Event::CarReachedParkingSpot(car, spot));
        let trip = &mut self.trips[self.active_trip_mode.remove(&AgentID::Car(car)).unwrap().0];

        match trip.legs.pop_front() {
            Some(TripLeg::Drive(vehicle, DrivingGoal::ParkNear(_))) => assert_eq!(car, vehicle.id),
            _ => unreachable!(),
        };

        match &trip.legs[0] {
            TripLeg::Walk(_, _, to) => match (spot, &to.connection) {
                (ParkingSpot::Offstreet(b1, _), SidewalkPOI::Building(b2)) if b1 == *b2 => {
                    // Do the relevant parts of ped_reached_parking_spot.
                    assert_eq!(trip.legs.len(), 1);
                    assert!(!trip.finished_at.is_some());
                    trip.finished_at = Some(now);
                    self.unfinished_trips -= 1;
                    self.events.push(Event::TripFinished(
                        trip.id,
                        trip.mode,
                        now - trip.spawned_at,
                    ));
                    self.people[trip.person.unwrap().0].state = PersonState::Inside(b1);
                    return;
                }
                _ => {}
            },
            _ => unreachable!(),
        };

        if !trip.spawn_ped(
            now,
            SidewalkSpot::parking_spot(spot, map, parking),
            map,
            scheduler,
        ) {
            self.unfinished_trips -= 1;
        }
    }

    pub fn ped_reached_parking_spot(
        &mut self,
        now: Time,
        ped: PedestrianID,
        spot: ParkingSpot,
        map: &Map,
        parking: &ParkingSimState,
        scheduler: &mut Scheduler,
    ) {
        self.events.push(Event::PedReachedParkingSpot(ped, spot));
        let trip = &mut self.trips[self
            .active_trip_mode
            .remove(&AgentID::Pedestrian(ped))
            .unwrap()
            .0];

        trip.assert_walking_leg(ped, SidewalkSpot::parking_spot(spot, map, parking));
        let (car, drive_to) = match trip.legs[0] {
            TripLeg::Drive(ref vehicle, ref to) => (vehicle.id, to.clone()),
            _ => unreachable!(),
        };
        let parked_car = parking.get_car_at_spot(spot).unwrap();
        assert_eq!(parked_car.vehicle.id, car);

        let mut start = parking.spot_to_driving_pos(parked_car.spot, &parked_car.vehicle, map);
        if let ParkingSpot::Offstreet(_, _) = spot {
            // Actually, to unpark, the car's front should be where it'll wind up at the end.
            start = Position::new(start.lane(), start.dist_along() + parked_car.vehicle.length);
        }
        let end = drive_to.goal_pos(PathConstraints::Car, map);
        let req = PathRequest {
            start,
            end,
            constraints: PathConstraints::Car,
        };
        let path = if let Some(p) = map.pathfind(req.clone()) {
            p
        } else {
            println!(
                "Aborting {} at {} because no path for the car portion! {} to {}",
                trip.id, now, start, end
            );
            self.unfinished_trips -= 1;
            trip.aborted = true;
            self.events.push(Event::TripAborted(trip.id, trip.mode));
            self.people[trip.person.unwrap().0].state = PersonState::Limbo;
            return;
        };

        let router = drive_to.make_router(path, map, parked_car.vehicle.vehicle_type);
        scheduler.push(
            now,
            Command::SpawnCar(
                CreateCar::for_parked_car(
                    parked_car.clone(),
                    router,
                    req,
                    start.dist_along(),
                    trip.id,
                ),
                true,
            ),
        );
    }

    pub fn ped_ready_to_bike(
        &mut self,
        now: Time,
        ped: PedestrianID,
        spot: SidewalkSpot,
        map: &Map,
        scheduler: &mut Scheduler,
    ) {
        let trip = &mut self.trips[self
            .active_trip_mode
            .remove(&AgentID::Pedestrian(ped))
            .unwrap()
            .0];

        trip.assert_walking_leg(ped, spot.clone());
        let (vehicle, drive_to) = match trip.legs[0] {
            TripLeg::Drive(ref vehicle, ref to) => (vehicle.clone(), to.clone()),
            _ => unreachable!(),
        };
        let driving_pos = match spot.connection {
            SidewalkPOI::BikeRack(p) => p,
            _ => unreachable!(),
        };

        let end = drive_to.goal_pos(PathConstraints::Bike, map);
        let req = PathRequest {
            start: driving_pos,
            end,
            constraints: PathConstraints::Bike,
        };
        let path = if let Some(p) = map.pathfind(req.clone()) {
            p
        } else {
            println!(
                "Aborting {} at {} because no path for the bike portion! {} to {}",
                trip.id, now, driving_pos, end
            );
            self.unfinished_trips -= 1;
            trip.aborted = true;
            self.events.push(Event::TripAborted(trip.id, trip.mode));
            self.people[trip.person.unwrap().0].state = PersonState::Limbo;
            return;
        };

        let router = drive_to.make_router(path, map, vehicle.vehicle_type);
        scheduler.push(
            now,
            Command::SpawnCar(
                CreateCar::for_appearing(vehicle, driving_pos, router, req, trip.id),
                true,
            ),
        );
    }

    pub fn bike_reached_end(
        &mut self,
        now: Time,
        bike: CarID,
        bike_rack: SidewalkSpot,
        map: &Map,
        scheduler: &mut Scheduler,
    ) {
        self.events.push(Event::BikeStoppedAtSidewalk(
            bike,
            bike_rack.sidewalk_pos.lane(),
        ));
        let trip = &mut self.trips[self.active_trip_mode.remove(&AgentID::Car(bike)).unwrap().0];

        match trip.legs.pop_front() {
            Some(TripLeg::Drive(vehicle, DrivingGoal::ParkNear(_))) => assert_eq!(vehicle.id, bike),
            _ => unreachable!(),
        };

        if !trip.spawn_ped(now, bike_rack, map, scheduler) {
            self.unfinished_trips -= 1;
        }
    }

    pub fn ped_reached_building(
        &mut self,
        now: Time,
        ped: PedestrianID,
        bldg: BuildingID,
        map: &Map,
    ) {
        self.events.push(Event::PedReachedBuilding(ped, bldg));
        let trip = &mut self.trips[self
            .active_trip_mode
            .remove(&AgentID::Pedestrian(ped))
            .unwrap()
            .0];
        trip.assert_walking_leg(ped, SidewalkSpot::building(bldg, map));
        assert!(trip.legs.is_empty());
        assert!(!trip.finished_at.is_some());
        trip.finished_at = Some(now);
        self.unfinished_trips -= 1;
        self.events.push(Event::TripFinished(
            trip.id,
            trip.mode,
            now - trip.spawned_at,
        ));
        self.people[trip.person.unwrap().0].state = PersonState::Inside(bldg);
    }

    // If no route is returned, the pedestrian boarded a bus immediately.
    pub fn ped_reached_bus_stop(
        &mut self,
        now: Time,
        ped: PedestrianID,
        stop: BusStopID,
        map: &Map,
        transit: &mut TransitSimState,
    ) -> Option<BusRouteID> {
        let trip = &mut self.trips[self.active_trip_mode[&AgentID::Pedestrian(ped)].0];
        match trip.legs[0] {
            TripLeg::Walk(p, _, ref spot) => {
                assert_eq!(p, ped);
                assert_eq!(*spot, SidewalkSpot::bus_stop(stop, map));
            }
            _ => unreachable!(),
        }
        match trip.legs[1] {
            TripLeg::RideBus(_, route, stop2) => {
                self.events.push(Event::PedReachedBusStop(ped, stop, route));
                self.events.push(Event::TripPhaseStarting(
                    trip.id,
                    trip.mode,
                    None,
                    TripPhaseType::WaitingForBus(route),
                ));
                if transit.ped_waiting_for_bus(now, ped, stop, route, stop2) {
                    trip.legs.pop_front();
                    None
                } else {
                    Some(route)
                }
            }
            _ => unreachable!(),
        }
    }

    pub fn ped_boarded_bus(
        &mut self,
        now: Time,
        ped: PedestrianID,
        walking: &mut WalkingSimState,
    ) -> TripID {
        // TODO Make sure canonical pt is the bus while the ped is riding it
        let trip = &mut self.trips[self.active_trip_mode[&AgentID::Pedestrian(ped)].0];
        trip.legs.pop_front();
        walking.ped_boarded_bus(now, ped);
        trip.id
    }

    pub fn ped_left_bus(
        &mut self,
        now: Time,
        ped: PedestrianID,
        map: &Map,
        scheduler: &mut Scheduler,
    ) {
        let trip = &mut self.trips[self
            .active_trip_mode
            .remove(&AgentID::Pedestrian(ped))
            .unwrap()
            .0];
        let start = match trip.legs.pop_front().unwrap() {
            TripLeg::RideBus(_, _, stop) => SidewalkSpot::bus_stop(stop, map),
            _ => unreachable!(),
        };

        if !trip.spawn_ped(now, start, map, scheduler) {
            self.unfinished_trips -= 1;
        }
    }

    pub fn ped_reached_border(
        &mut self,
        now: Time,
        ped: PedestrianID,
        i: IntersectionID,
        map: &Map,
    ) {
        self.events.push(Event::PedReachedBorder(ped, i));
        let trip = &mut self.trips[self
            .active_trip_mode
            .remove(&AgentID::Pedestrian(ped))
            .unwrap()
            .0];
        trip.assert_walking_leg(ped, SidewalkSpot::end_at_border(i, map).unwrap());
        assert!(trip.legs.is_empty());
        assert!(!trip.finished_at.is_some());
        trip.finished_at = Some(now);
        self.unfinished_trips -= 1;
        self.events.push(Event::TripFinished(
            trip.id,
            trip.mode,
            now - trip.spawned_at,
        ));
        self.people[trip.person.unwrap().0].state = PersonState::OffMap;
    }

    pub fn car_or_bike_reached_border(&mut self, now: Time, car: CarID, i: IntersectionID) {
        self.events.push(Event::CarOrBikeReachedBorder(car, i));
        let trip = &mut self.trips[self.active_trip_mode.remove(&AgentID::Car(car)).unwrap().0];
        match trip.legs.pop_front().unwrap() {
            TripLeg::Drive(_, DrivingGoal::Border(int, _)) => assert_eq!(i, int),
            _ => unreachable!(),
        };
        assert!(trip.legs.is_empty());
        assert!(!trip.finished_at.is_some());
        trip.finished_at = Some(now);
        self.unfinished_trips -= 1;
        self.events.push(Event::TripFinished(
            trip.id,
            trip.mode,
            now - trip.spawned_at,
        ));
        self.people[trip.person.unwrap().0].state = PersonState::OffMap;
    }

    pub fn abort_trip_failed_start(&mut self, id: TripID) {
        let trip = &mut self.trips[id.0];
        trip.aborted = true;
        if !trip.is_bus_trip() {
            self.unfinished_trips -= 1;
            // TODO Urgh, hack. Initialization order is now quite complicated.
            if let Some(p) = trip.person {
                self.people[p.0].state = PersonState::Limbo;
            }
        }
        self.events.push(Event::TripAborted(id, trip.mode));
    }

    pub fn abort_trip_impossible_parking(&mut self, car: CarID) {
        let id = self.active_trip_mode.remove(&AgentID::Car(car)).unwrap();
        let trip = &mut self.trips[id.0];
        assert!(!trip.is_bus_trip());
        trip.aborted = true;
        self.unfinished_trips -= 1;
        self.events.push(Event::TripAborted(trip.id, trip.mode));
        self.people[trip.person.unwrap().0].state = PersonState::Limbo;
    }

    pub fn active_agents(&self) -> Vec<AgentID> {
        self.active_trip_mode.keys().cloned().collect()
    }

    pub fn get_active_trips(&self) -> Vec<TripID> {
        self.active_trip_mode.values().cloned().collect()
    }

    pub fn trip_to_agent(&self, id: TripID) -> TripResult<AgentID> {
        if id.0 >= self.trips.len() {
            return TripResult::TripDoesntExist;
        }
        let trip = &self.trips[id.0];

        if trip.finished_at.is_some() || trip.aborted {
            return TripResult::TripDone;
        }

        match &trip.legs[0] {
            TripLeg::Walk(id, _, _) => TripResult::Ok(AgentID::Pedestrian(*id)),
            TripLeg::Drive(vehicle, _) => TripResult::Ok(AgentID::Car(vehicle.id)),
            // TODO Should be the bus, but apparently transit sim tracks differently?
            TripLeg::RideBus(ped, _, _) => TripResult::Ok(AgentID::Pedestrian(*ped)),
            TripLeg::ServeBusRoute(id, _) => TripResult::Ok(AgentID::Car(*id)),
        }
    }

    // This will be None for parked cars
    pub fn agent_to_trip(&self, id: AgentID) -> Option<TripID> {
        self.active_trip_mode.get(&id).cloned()
    }

    pub fn debug_trip(&self, id: AgentID) {
        if let Some(t) = self.active_trip_mode.get(&id) {
            let trip = &self.trips[t.0];
            println!("{} has goal {:?}", trip.id, trip.legs.back().unwrap());
        } else {
            println!("{} has no trip, must be parked car", id);
        }
    }

    // (finished trips, unfinished trips, active trips by the trip's current mode, people in
    // buildings, people off map)
    pub fn num_trips(&self) -> (usize, usize, BTreeMap<TripMode, usize>, usize, usize) {
        let mut cnt = Counter::new();
        for a in self.active_trip_mode.keys() {
            cnt.inc(TripMode::from_agent(*a));
        }
        let per_mode = TripMode::all()
            .into_iter()
            .map(|k| (k, cnt.get(k)))
            .collect();
        let mut ppl_in_bldg = 0;
        let mut ppl_off_map = 0;
        for p in &self.people {
            match p.state {
                PersonState::Trip(_) => {}
                PersonState::Inside(_) => {
                    ppl_in_bldg += 1;
                }
                PersonState::OffMap => {
                    ppl_off_map += 1;
                }
                PersonState::Limbo => {}
            }
        }
        (
            self.trips.len() - self.unfinished_trips,
            self.unfinished_trips,
            per_mode,
            ppl_in_bldg,
            ppl_off_map,
        )
    }

    pub fn is_done(&self) -> bool {
        self.unfinished_trips == 0
    }

    pub fn collect_events(&mut self) -> Vec<Event> {
        std::mem::replace(&mut self.events, Vec::new())
    }

    // Return trip start time too
    pub fn find_trip_using_car(&self, id: CarID, home: BuildingID) -> Option<(TripID, Time)> {
        let t = self.trips.iter().find(|t| t.uses_car(id, home))?;
        Some((t.id, t.spawned_at))
    }

    pub fn trip_endpoints(&self, id: TripID) -> (TripStart, TripEnd) {
        let t = &self.trips[id.0];
        (t.start.clone(), t.end.clone())
    }

    // TODO Refactor after wrangling the TripStart/TripEnd mess
    pub fn count_trips_involving_bldg(&self, b: BuildingID, now: Time) -> TripCount {
        self.count_trips(TripStart::Bldg(b), TripEnd::Bldg(b), now)
    }
    pub fn count_trips_involving_border(&self, i: IntersectionID, now: Time) -> TripCount {
        self.count_trips(TripStart::Border(i), TripEnd::Border(i), now)
    }
    fn count_trips(&self, start: TripStart, end: TripEnd, now: Time) -> TripCount {
        let mut cnt = TripCount {
            from_aborted: Vec::new(),
            from_in_progress: Vec::new(),
            from_completed: Vec::new(),
            from_unstarted: Vec::new(),
            to_aborted: Vec::new(),
            to_in_progress: Vec::new(),
            to_completed: Vec::new(),
            to_unstarted: Vec::new(),
        };
        for trip in &self.trips {
            if trip.start == start {
                if trip.aborted {
                    cnt.from_aborted.push(trip.id);
                } else if trip.finished_at.is_some() {
                    cnt.from_completed.push(trip.id);
                } else if now >= trip.spawned_at {
                    cnt.from_in_progress.push(trip.id);
                } else {
                    cnt.from_unstarted.push(trip.id);
                }
            }
            // One trip might could towards both!
            if trip.end == end {
                if trip.aborted {
                    cnt.to_aborted.push(trip.id);
                } else if trip.finished_at.is_some() {
                    cnt.to_completed.push(trip.id);
                } else if now >= trip.spawned_at {
                    cnt.to_in_progress.push(trip.id);
                } else {
                    cnt.to_unstarted.push(trip.id);
                }
            }
        }
        cnt
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
struct Trip {
    id: TripID,
    spawned_at: Time,
    finished_at: Option<Time>,
    aborted: bool,
    legs: VecDeque<TripLeg>,
    mode: TripMode,
    start: TripStart,
    end: TripEnd,
    // None for bus trips.
    person: Option<PersonID>,
}

impl Trip {
    fn uses_car(&self, id: CarID, home: BuildingID) -> bool {
        self.legs.iter().any(|l| match l {
            TripLeg::Walk(_, _, ref walk_to) => match walk_to.connection {
                SidewalkPOI::DeferredParkingSpot(b, _) => b == home,
                _ => false,
            },
            // No need to look up the contents of a SidewalkPOI::ParkingSpot. If a trip uses a
            // specific parked car, then there'll be a TripLeg::Drive with it already.
            TripLeg::Drive(ref vehicle, _) => vehicle.id == id,
            _ => false,
        })
    }

    fn is_bus_trip(&self) -> bool {
        self.legs.len() == 1
            && match self.legs[0] {
                TripLeg::ServeBusRoute(_, _) => true,
                _ => false,
            }
    }

    // Returns true if this succeeds. If not, trip aborted.
    fn spawn_ped(
        &self,
        now: Time,
        start: SidewalkSpot,
        map: &Map,
        scheduler: &mut Scheduler,
    ) -> bool {
        let (ped, speed, walk_to) = match self.legs[0] {
            TripLeg::Walk(ped, speed, ref to) => (ped, speed, to.clone()),
            _ => unreachable!(),
        };

        let req = PathRequest {
            start: start.sidewalk_pos,
            end: walk_to.sidewalk_pos,
            constraints: PathConstraints::Pedestrian,
        };
        let path = if let Some(p) = map.pathfind(req.clone()) {
            p
        } else {
            println!(
                "Aborting {} at {} because no path for the walking portion! {:?} to {:?}",
                self.id, now, start, walk_to
            );
            return false;
        };

        scheduler.push(
            now,
            Command::SpawnPed(CreatePedestrian {
                id: ped,
                speed,
                start,
                goal: walk_to,
                path,
                req,
                trip: self.id,
            }),
        );
        true
    }

    fn assert_walking_leg(&mut self, ped: PedestrianID, goal: SidewalkSpot) {
        match self.legs.pop_front() {
            Some(TripLeg::Walk(p, _, spot)) => {
                assert_eq!(ped, p);
                assert_eq!(goal, spot);
            }
            _ => unreachable!(),
        }
    }
}

// These don't specify where the leg starts, since it might be unknown -- like when we drive and
// don't know where we'll wind up parking.
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum TripLeg {
    Walk(PedestrianID, Speed, SidewalkSpot),
    Drive(Vehicle, DrivingGoal),
    RideBus(PedestrianID, BusRouteID, BusStopID),
    ServeBusRoute(CarID, BusRouteID),
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Copy, PartialOrd, Ord)]
pub enum TripMode {
    Walk,
    Bike,
    Transit,
    Drive,
}

impl TripMode {
    pub fn all() -> Vec<TripMode> {
        vec![
            TripMode::Walk,
            TripMode::Bike,
            TripMode::Transit,
            TripMode::Drive,
        ]
    }

    pub fn from_agent(id: AgentID) -> TripMode {
        match id {
            AgentID::Pedestrian(_) => TripMode::Walk,
            AgentID::Car(id) => match id.1 {
                VehicleType::Car => TripMode::Drive,
                VehicleType::Bike => TripMode::Bike,
                VehicleType::Bus => TripMode::Transit,
            },
        }
    }
}

impl std::fmt::Display for TripMode {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            TripMode::Walk => write!(f, "walk"),
            TripMode::Bike => write!(f, "bike"),
            TripMode::Transit => write!(f, "transit"),
            TripMode::Drive => write!(f, "drive"),
        }
    }
}

// TODO Argh no, not more of these variants!

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum TripStart {
    Bldg(BuildingID),
    Border(IntersectionID),
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum TripEnd {
    Bldg(BuildingID),
    Border(IntersectionID),
    // No end!
    ServeBusRoute(BusRouteID),
}

pub enum TripResult<T> {
    Ok(T),
    ModeChange,
    TripDone,
    TripDoesntExist,
}

impl<T> TripResult<T> {
    pub fn ok(self) -> Option<T> {
        match self {
            TripResult::Ok(data) => Some(data),
            _ => None,
        }
    }

    pub fn propagate_error<X>(self) -> TripResult<X> {
        match self {
            TripResult::Ok(_) => panic!("TripResult is Ok, can't propagate_error"),
            TripResult::ModeChange => TripResult::ModeChange,
            TripResult::TripDone => TripResult::TripDone,
            TripResult::TripDoesntExist => TripResult::TripDoesntExist,
        }
    }
}

// TODO Misnomer now
pub struct TripCount {
    pub from_aborted: Vec<TripID>,
    pub from_in_progress: Vec<TripID>,
    pub from_completed: Vec<TripID>,
    pub from_unstarted: Vec<TripID>,
    pub to_aborted: Vec<TripID>,
    pub to_in_progress: Vec<TripID>,
    pub to_completed: Vec<TripID>,
    pub to_unstarted: Vec<TripID>,
}

impl TripCount {
    pub fn describe(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if !self.from_completed.is_empty() || !self.to_completed.is_empty() {
            lines.push(format!(
                "Finished trips: {} from here, {} to here",
                self.from_completed.len(),
                self.to_completed.len()
            ));
        }
        if !self.from_in_progress.is_empty() || !self.to_in_progress.is_empty() {
            lines.push(format!(
                "In-progress trips: {} from here, {} to here",
                self.from_in_progress.len(),
                self.to_in_progress.len()
            ));
        }
        if !self.from_unstarted.is_empty() || !self.to_unstarted.is_empty() {
            lines.push(format!(
                "Future trips: {} from here, {} to here",
                self.from_unstarted.len(),
                self.to_unstarted.len()
            ));
        }
        if !self.from_aborted.is_empty() || !self.to_aborted.is_empty() {
            lines.push(format!(
                "Aborted trips: {} from here, {} to here",
                self.from_aborted.len(),
                self.to_aborted.len()
            ));
        }
        lines
    }
}

// TODO General weirdness right now: we operate based on trips, and the people are side-effects. So
// one "person" might start their second trip before finishing their first.

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Person {
    pub id: PersonID,
    pub trips: Vec<TripID>,
    // TODO home
    pub state: PersonState,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum PersonState {
    Trip(TripID),
    Inside(BuildingID),
    OffMap,
    // One of the trips was aborted. Silently eat them from the exposed counters. :\
    Limbo,
}
