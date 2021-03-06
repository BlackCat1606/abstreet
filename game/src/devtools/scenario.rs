use crate::app::App;
use crate::colors;
use crate::common::{tool_panel, Colorer, CommonState, Warping};
use crate::game::{State, Transition, WizardState};
use crate::helpers::ID;
use crate::managed::{WrappedComposite, WrappedOutcome};
use abstutil::{prettyprint_usize, Counter, MultiMap};
use ezgui::{
    hotkey, lctrl, Choice, Color, Composite, Drawable, EventCtx, GeomBatch, GfxCtx,
    HorizontalAlignment, Key, Line, ManagedWidget, Outcome, Slider, Text, VerticalAlignment,
};
use geom::{Distance, Line, PolyLine, Polygon};
use map_model::{BuildingID, IntersectionID, Map};
use sim::{DrivingGoal, IndividTrip, Scenario, SidewalkPOI, SidewalkSpot, SpawnTrip};
use std::collections::BTreeSet;

pub struct ScenarioManager {
    composite: Composite,
    common: CommonState,
    tool_panel: WrappedComposite,
    scenario: Scenario,

    // The usizes are indices into scenario.population.individ_trips
    trips_from_bldg: MultiMap<BuildingID, usize>,
    trips_to_bldg: MultiMap<BuildingID, usize>,
    trips_from_border: MultiMap<IntersectionID, usize>,
    trips_to_border: MultiMap<IntersectionID, usize>,
    bldg_colors: Colorer,

    demand: Option<Drawable>,
}

impl ScenarioManager {
    pub fn new(scenario: Scenario, ctx: &mut EventCtx, app: &App) -> ScenarioManager {
        let mut trips_from_bldg = MultiMap::new();
        let mut trips_to_bldg = MultiMap::new();
        let mut trips_from_border = MultiMap::new();
        let mut trips_to_border = MultiMap::new();
        for (idx, trip) in scenario.population.individ_trips.iter().enumerate() {
            // trips_from_bldg and trips_from_border
            match &trip.trip {
                // TODO CarAppearing might be from a border
                SpawnTrip::CarAppearing { .. } => {}
                SpawnTrip::MaybeUsingParkedCar(b, _) => {
                    trips_from_bldg.insert(*b, idx);
                }
                SpawnTrip::UsingBike(ref spot, _)
                | SpawnTrip::JustWalking(ref spot, _)
                | SpawnTrip::UsingTransit(ref spot, _, _, _, _) => match spot.connection {
                    SidewalkPOI::Building(b) => {
                        trips_from_bldg.insert(b, idx);
                    }
                    SidewalkPOI::Border(i) => {
                        trips_from_border.insert(i, idx);
                    }
                    _ => {}
                },
            }

            // trips_to_bldg and trips_to_border
            match trip.trip {
                SpawnTrip::CarAppearing { ref goal, .. }
                | SpawnTrip::MaybeUsingParkedCar(_, ref goal)
                | SpawnTrip::UsingBike(_, ref goal) => match goal {
                    DrivingGoal::ParkNear(b) => {
                        trips_to_bldg.insert(*b, idx);
                    }
                    DrivingGoal::Border(i, _) => {
                        trips_to_border.insert(*i, idx);
                    }
                },
                SpawnTrip::JustWalking(_, ref spot)
                | SpawnTrip::UsingTransit(_, ref spot, _, _, _) => match spot.connection {
                    SidewalkPOI::Building(b) => {
                        trips_to_bldg.insert(b, idx);
                    }
                    SidewalkPOI::Border(i) => {
                        trips_to_border.insert(i, idx);
                    }
                    _ => {}
                },
            }
        }

        let mut bldg_colors = Colorer::new(
            Text::from(Line("buildings")),
            vec![
                ("1-2 cars needed", Color::BLUE),
                ("3-4 cars needed", Color::RED),
                (">= 5 cars needed", Color::BLACK),
            ],
        );
        let mut total_cars_needed = 0;
        for (b, count) in &scenario.population.individ_parked_cars {
            total_cars_needed += count;
            let color = if *count == 0 {
                continue;
            } else if *count == 1 || *count == 2 {
                Color::BLUE
            } else if *count == 3 || *count == 4 {
                Color::RED
            } else {
                Color::BLACK
            };
            bldg_colors.add_b(*b, color);
        }

        let (filled_spots, free_parking_spots) = app.primary.sim.get_all_parking_spots();
        assert!(filled_spots.is_empty());

        ScenarioManager {
            composite: WrappedComposite::quick_menu(
                ctx,
                format!("Scenario {}", scenario.scenario_name),
                vec![
                    format!(
                        "{} total trips",
                        prettyprint_usize(scenario.population.individ_trips.len())
                    ),
                    format!(
                        "{} people",
                        prettyprint_usize(scenario.population.people.len())
                    ),
                    format!("seed {} parked cars", prettyprint_usize(total_cars_needed)),
                    format!(
                        "{} parking spots",
                        prettyprint_usize(free_parking_spots.len()),
                    ),
                ],
                vec![
                    (hotkey(Key::D), "dot map"),
                    (lctrl(Key::P), "stop showing paths"),
                ],
            ),
            common: CommonState::new(),
            tool_panel: tool_panel(ctx),
            scenario,
            trips_from_bldg,
            trips_to_bldg,
            trips_from_border,
            trips_to_border,
            bldg_colors: bldg_colors.build(ctx, app),
            demand: None,
        }
    }
}

impl State for ScenarioManager {
    fn event(&mut self, ctx: &mut EventCtx, app: &mut App) -> Transition {
        match self.composite.event(ctx) {
            Some(Outcome::Clicked(x)) => match x.as_ref() {
                "X" => {
                    return Transition::Pop;
                }
                "dot map" => {
                    return Transition::Push(Box::new(DotMap::new(ctx, app, &self.scenario)));
                }
                // TODO Inactivate this sometimes
                "stop showing paths" => {
                    self.demand = None;
                }
                _ => unreachable!(),
            },
            None => {}
        }

        ctx.canvas_movement();
        if ctx.redo_mouseover() {
            app.recalculate_current_selection(ctx);
        }

        if let Some(ID::Building(b)) = app.primary.current_selection {
            let from = self.trips_from_bldg.get(b);
            let to = self.trips_to_bldg.get(b);
            if !from.is_empty() || !to.is_empty() {
                if app.per_obj.action(ctx, Key::T, "browse trips") {
                    // TODO Avoid the clone? Just happens once though.
                    let mut all_trips = from.clone();
                    all_trips.extend(to);

                    return Transition::Push(make_trip_picker(
                        self.scenario.clone(),
                        all_trips,
                        "building",
                        OD::Bldg(b),
                    ));
                } else if self.demand.is_none()
                    && app.per_obj.action(ctx, Key::P, "show trips to and from")
                {
                    self.demand =
                        Some(show_demand(&self.scenario, from, to, OD::Bldg(b), app, ctx));
                }
            }
        } else if let Some(ID::Intersection(i)) = app.primary.current_selection {
            let from = self.trips_from_border.get(i);
            let to = self.trips_to_border.get(i);
            if !from.is_empty() || !to.is_empty() {
                if app.per_obj.action(ctx, Key::T, "browse trips") {
                    // TODO Avoid the clone? Just happens once though.
                    let mut all_trips = from.clone();
                    all_trips.extend(to);

                    return Transition::Push(make_trip_picker(
                        self.scenario.clone(),
                        all_trips,
                        "border",
                        OD::Border(i),
                    ));
                } else if self.demand.is_none()
                    && app.per_obj.action(ctx, Key::P, "show trips to and from")
                {
                    self.demand = Some(show_demand(
                        &self.scenario,
                        from,
                        to,
                        OD::Border(i),
                        app,
                        ctx,
                    ));
                }
            }
        }

        if let Some(t) = self.common.event(ctx, app, None) {
            return t;
        }
        match self.tool_panel.event(ctx, app) {
            Some(WrappedOutcome::Transition(t)) => t,
            Some(WrappedOutcome::Clicked(x)) => match x.as_ref() {
                "back" => Transition::Pop,
                _ => unreachable!(),
            },
            None => Transition::Keep,
        }
    }

    fn draw(&self, g: &mut GfxCtx, app: &App) {
        // TODO Let common contribute draw_options...
        self.bldg_colors.draw(g);
        if let Some(ref p) = self.demand {
            g.redraw(p);
        }

        self.composite.draw(g);
        self.common.draw_no_osd(g, app);
        self.tool_panel.draw(g);

        if let Some(ID::Building(b)) = app.primary.current_selection {
            let mut osd = CommonState::default_osd(ID::Building(b), app);
            osd.append(Line(format!(
                ". {} trips from here, {} trips to here, {} parked cars needed",
                self.trips_from_bldg.get(b).len(),
                self.trips_to_bldg.get(b).len(),
                self.scenario.population.individ_parked_cars[&b]
            )));
            CommonState::draw_custom_osd(g, app, osd);
        } else if let Some(ID::Intersection(i)) = app.primary.current_selection {
            let mut osd = CommonState::default_osd(ID::Intersection(i), app);
            osd.append(Line(format!(
                ". {} trips from here, {} trips to here",
                self.trips_from_border.get(i).len(),
                self.trips_to_border.get(i).len(),
            )));
            CommonState::draw_custom_osd(g, app, osd);
        } else {
            CommonState::draw_osd(g, app, &app.primary.current_selection);
        }
    }
}

// TODO Yet another one of these... something needs to change.
#[derive(PartialEq, Debug, Clone, Copy)]
enum OD {
    Bldg(BuildingID),
    Border(IntersectionID),
}

fn make_trip_picker(
    scenario: Scenario,
    indices: BTreeSet<usize>,
    noun: &'static str,
    home: OD,
) -> Box<dyn State> {
    WizardState::new(Box::new(move |wiz, ctx, app| {
        let mut people = BTreeSet::new();
        for idx in &indices {
            people.insert(scenario.population.individ_trips[*idx].person);
        }

        let warp_to = wiz
            .wrap(ctx)
            .choose(
                &format!("Trips from/to this {}, by {} people", noun, people.len()),
                || {
                    // TODO Panics if there are two duplicate trips (b1124 in montlake)
                    indices
                        .iter()
                        .map(|idx| {
                            let trip = &scenario.population.individ_trips[*idx];
                            Choice::new(
                                describe(trip, home),
                                other_endpt(trip, home, &app.primary.map),
                            )
                        })
                        .collect()
                },
            )?
            .1;
        Some(Transition::Replace(Warping::new(
            ctx,
            warp_to.canonical_point(&app.primary).unwrap(),
            None,
            Some(warp_to),
            &mut app.primary,
        )))
    }))
}

fn describe(trip: &IndividTrip, home: OD) -> String {
    let driving_goal = |goal: &DrivingGoal| match goal {
        DrivingGoal::ParkNear(b) => {
            if OD::Bldg(*b) == home {
                "HERE".to_string()
            } else {
                b.to_string()
            }
        }
        DrivingGoal::Border(i, _) => {
            if OD::Border(*i) == home {
                "HERE".to_string()
            } else {
                i.to_string()
            }
        }
    };
    let sidewalk_spot = |spot: &SidewalkSpot| match &spot.connection {
        SidewalkPOI::Building(b) => {
            if OD::Bldg(*b) == home {
                "HERE".to_string()
            } else {
                b.to_string()
            }
        }
        SidewalkPOI::Border(i) => {
            if OD::Border(*i) == home {
                "HERE".to_string()
            } else {
                i.to_string()
            }
        }
        x => format!("{:?}", x),
    };

    match &trip.trip {
        SpawnTrip::CarAppearing {
            start,
            goal,
            is_bike,
        } => format!(
            "{} at {}: {} appears at {}, goes to {}",
            trip.person,
            trip.depart,
            if *is_bike { "bike" } else { "car" },
            start.lane(),
            driving_goal(goal)
        ),
        SpawnTrip::MaybeUsingParkedCar(start_bldg, goal) => format!(
            "{} at {}: try to drive from {} to {}",
            trip.person,
            trip.depart,
            if OD::Bldg(*start_bldg) == home {
                "HERE".to_string()
            } else {
                start_bldg.to_string()
            },
            driving_goal(goal),
        ),
        SpawnTrip::UsingBike(start, goal) => format!(
            "{} at {}: bike from {} to {}",
            trip.person,
            trip.depart,
            sidewalk_spot(start),
            driving_goal(goal)
        ),
        SpawnTrip::JustWalking(start, goal) => format!(
            "{} at {}: walk from {} to {}",
            trip.person,
            trip.depart,
            sidewalk_spot(start),
            sidewalk_spot(goal)
        ),
        SpawnTrip::UsingTransit(start, goal, route, _, _) => format!(
            "{} at {}: bus from {} to {} using {}",
            trip.person,
            trip.depart,
            sidewalk_spot(start),
            sidewalk_spot(goal),
            route
        ),
    }
}

fn other_endpt(trip: &IndividTrip, home: OD, map: &Map) -> ID {
    let driving_goal = |goal: &DrivingGoal| match goal {
        DrivingGoal::ParkNear(b) => ID::Building(*b),
        DrivingGoal::Border(i, _) => ID::Intersection(*i),
    };
    let sidewalk_spot = |spot: &SidewalkSpot| match &spot.connection {
        SidewalkPOI::Building(b) => ID::Building(*b),
        SidewalkPOI::Border(i) => ID::Intersection(*i),
        x => panic!("other_endpt for {:?}?", x),
    };

    let (from, to) = match &trip.trip {
        SpawnTrip::CarAppearing { start, goal, .. } => (
            ID::Intersection(map.get_l(start.lane()).src_i),
            driving_goal(goal),
        ),
        SpawnTrip::MaybeUsingParkedCar(start_bldg, goal) => {
            (ID::Building(*start_bldg), driving_goal(goal))
        }
        SpawnTrip::UsingBike(start, goal) => (sidewalk_spot(start), driving_goal(goal)),
        SpawnTrip::JustWalking(start, goal) => (sidewalk_spot(start), sidewalk_spot(goal)),
        SpawnTrip::UsingTransit(start, goal, _, _, _) => {
            (sidewalk_spot(start), sidewalk_spot(goal))
        }
    };
    let home_id = match home {
        OD::Bldg(b) => ID::Building(b),
        OD::Border(i) => ID::Intersection(i),
    };
    if from == home_id {
        to
    } else if to == home_id {
        from
    } else {
        panic!("other_endpt broke when homed at {:?} for {:?}", home, trip)
    }
}

// TODO Understand demand better.
// - Be able to select an area, see trips to/from it
// - Weight the arrow size by how many trips go there
// - Legend, counting the number of trips
fn show_demand(
    scenario: &Scenario,
    from: &BTreeSet<usize>,
    to: &BTreeSet<usize>,
    home: OD,
    app: &App,
    ctx: &EventCtx,
) -> Drawable {
    let mut from_ids = Counter::new();
    for idx in from {
        from_ids.inc(other_endpt(
            &scenario.population.individ_trips[*idx],
            home,
            &app.primary.map,
        ));
    }
    let mut to_ids = Counter::new();
    for idx in to {
        to_ids.inc(other_endpt(
            &scenario.population.individ_trips[*idx],
            home,
            &app.primary.map,
        ));
    }
    let from_count = from_ids.consume();
    let mut to_count = to_ids.consume();
    let max_count =
        (*from_count.values().max().unwrap()).max(*to_count.values().max().unwrap()) as f64;

    let mut batch = GeomBatch::new();
    let home_pt = match home {
        OD::Bldg(b) => app.primary.map.get_b(b).polygon.center(),
        OD::Border(i) => app.primary.map.get_i(i).polygon.center(),
    };

    for (id, cnt) in from_count {
        // Bidirectional?
        if let Some(other_cnt) = to_count.remove(&id) {
            let width = Distance::meters(1.0)
                + ((cnt.max(other_cnt) as f64) / max_count) * Distance::meters(2.0);
            batch.push(
                Color::PURPLE.alpha(0.8),
                PolyLine::new(vec![home_pt, id.canonical_point(&app.primary).unwrap()])
                    .make_polygons(width),
            );
        } else {
            let width = Distance::meters(1.0) + ((cnt as f64) / max_count) * Distance::meters(2.0);
            batch.push(
                Color::RED.alpha(0.8),
                PolyLine::new(vec![home_pt, id.canonical_point(&app.primary).unwrap()])
                    .make_arrow(width)
                    .unwrap(),
            );
        }
    }
    for (id, cnt) in to_count {
        let width = Distance::meters(1.0) + ((cnt as f64) / max_count) * Distance::meters(2.0);
        batch.push(
            Color::BLUE.alpha(0.8),
            PolyLine::new(vec![id.canonical_point(&app.primary).unwrap(), home_pt])
                .make_arrow(width)
                .unwrap(),
        );
    }

    batch.upload(ctx)
}

struct DotMap {
    composite: Composite,

    lines: Vec<Line>,
    draw: Option<(f64, Drawable)>,
}

impl DotMap {
    fn new(ctx: &mut EventCtx, app: &App, scenario: &Scenario) -> DotMap {
        let map = &app.primary.map;
        let lines = scenario
            .population
            .individ_trips
            .iter()
            .filter_map(|trip| {
                let (start, end) = match &trip.trip {
                    SpawnTrip::CarAppearing { start, goal, .. } => (start.pt(map), goal.pt(map)),
                    SpawnTrip::MaybeUsingParkedCar(b, goal) => {
                        (map.get_b(*b).polygon.center(), goal.pt(map))
                    }
                    SpawnTrip::UsingBike(start, goal) => (start.sidewalk_pos.pt(map), goal.pt(map)),
                    SpawnTrip::JustWalking(start, goal) => {
                        (start.sidewalk_pos.pt(map), goal.sidewalk_pos.pt(map))
                    }
                    SpawnTrip::UsingTransit(start, goal, _, _, _) => {
                        (start.sidewalk_pos.pt(map), goal.sidewalk_pos.pt(map))
                    }
                };
                Line::maybe_new(start, end)
            })
            .collect();
        DotMap {
            composite: Composite::new(
                ManagedWidget::col(vec![
                    ManagedWidget::row(vec![
                        ManagedWidget::draw_text(
                            ctx,
                            Text::from(Line("Dot map of all trips").roboto_bold()),
                        ),
                        WrappedComposite::text_button(ctx, "X", hotkey(Key::Escape)).align_right(),
                    ]),
                    ManagedWidget::slider("time slider"),
                ])
                .padding(10)
                .bg(colors::PANEL_BG),
            )
            .aligned(HorizontalAlignment::Center, VerticalAlignment::Top)
            .slider("time slider", Slider::horizontal(ctx, 150.0, 25.0))
            .build(ctx),

            lines,
            draw: None,
        }
    }
}

impl State for DotMap {
    fn event(&mut self, ctx: &mut EventCtx, _: &mut App) -> Transition {
        ctx.canvas_movement();

        match self.composite.event(ctx) {
            Some(Outcome::Clicked(x)) => match x.as_ref() {
                "X" => {
                    return Transition::Pop;
                }
                _ => unreachable!(),
            },
            None => {}
        }

        let pct = self.composite.slider("time slider").get_percent();

        if self.draw.as_ref().map(|(p, _)| pct != *p).unwrap_or(true) {
            let mut batch = GeomBatch::new();
            let radius = Distance::meters(5.0);
            for l in &self.lines {
                // Circles are too expensive. :P
                batch.push(
                    Color::RED,
                    Polygon::rectangle_centered(l.percent_along(pct), radius, radius),
                );
            }
            self.draw = Some((pct, batch.upload(ctx)));
        }

        Transition::Keep
    }

    fn draw(&self, g: &mut GfxCtx, _: &App) {
        if let Some((_, ref d)) = self.draw {
            g.redraw(d);
        }
        self.composite.draw(g);
    }
}
