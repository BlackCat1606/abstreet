use crate::{AgentID, CarID, ParkingSpot, PedestrianID, TripID, TripMode};
use geom::Duration;
use map_model::{BuildingID, BusRouteID, BusStopID, IntersectionID, LaneID, Traversable};
use serde_derive::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    CarReachedParkingSpot(CarID, ParkingSpot),
    CarOrBikeVanished(CarID, LaneID),

    BusArrivedAtStop(CarID, BusRouteID, BusStopID),
    BusDepartedFromStop(CarID, BusRouteID, BusStopID),

    PedReachedParkingSpot(PedestrianID, ParkingSpot),
    PedReachedBuilding(PedestrianID, BuildingID),
    PedReachedBorder(PedestrianID, IntersectionID),
    PedReachedBusStop(PedestrianID, BusStopID),
    PedEntersBus(PedestrianID, CarID, BusRouteID),
    PedLeavesBus(PedestrianID, CarID, BusRouteID),

    BikeStoppedAtSidewalk(CarID, LaneID),

    AgentEntersTraversable(AgentID, Traversable),

    TripFinished(TripID, TripMode, Duration),
    TripAborted(TripID),
}
