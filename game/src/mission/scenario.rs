use crate::common::{CommonState, ObjectColorer, ObjectColorerBuilder, Warping};
use crate::game::{State, Transition, WizardState};
use crate::helpers::ID;
use crate::mission::pick_time_range;
use crate::sandbox::{GameplayMode, SandboxMode};
use crate::ui::{ShowEverything, UI};
use abstutil::{prettyprint_usize, Counter, MultiMap, WeightedUsizeChoice};
use ezgui::{
    hotkey, Choice, Color, Drawable, EventCtx, EventLoopMode, GeomBatch, GfxCtx, Key, Line,
    MenuUnderButton, ModalMenu, Text, Wizard, WrappedWizard,
};
use geom::{Distance, Duration, PolyLine};
use map_model::{BuildingID, IntersectionID, Map, Neighborhood};
use sim::{
    BorderSpawnOverTime, OriginDestination, Scenario, SeedParkedCars, SidewalkPOI, SidewalkSpot,
    SpawnOverTime, SpawnTrip, TripEndpoint,
};
use std::collections::BTreeSet;

pub struct ScenarioManager {
    menu: ModalMenu,
    general_tools: MenuUnderButton,
    common: CommonState,
    scenario: Scenario,

    // The usizes are indices into scenario.individ_trips
    trips_from_bldg: MultiMap<BuildingID, usize>,
    trips_to_bldg: MultiMap<BuildingID, usize>,
    trips_from_border: MultiMap<IntersectionID, usize>,
    trips_to_border: MultiMap<IntersectionID, usize>,
    total_cars_needed: usize,
    total_parking_spots: usize,
    bldg_colors: ObjectColorer,

    demand: Option<Drawable>,
}

impl ScenarioManager {
    pub fn new(scenario: Scenario, ctx: &mut EventCtx, ui: &UI) -> ScenarioManager {
        let mut trips_from_bldg = MultiMap::new();
        let mut trips_to_bldg = MultiMap::new();
        let mut trips_from_border = MultiMap::new();
        let mut trips_to_border = MultiMap::new();
        for (idx, trip) in scenario.individ_trips.iter().enumerate() {
            // trips_from_bldg and trips_from_border
            match trip {
                // TODO CarAppearing might be from a border
                SpawnTrip::CarAppearing { .. } => {}
                SpawnTrip::MaybeUsingParkedCar(_, b, _) => {
                    trips_from_bldg.insert(*b, idx);
                }
                SpawnTrip::UsingBike(_, ref spot, _)
                | SpawnTrip::JustWalking(_, ref spot, _)
                | SpawnTrip::UsingTransit(_, ref spot, _, _, _, _) => match spot.connection {
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
            match trip {
                SpawnTrip::CarAppearing { ref goal, .. }
                | SpawnTrip::MaybeUsingParkedCar(_, _, ref goal)
                | SpawnTrip::UsingBike(_, _, ref goal) => match goal {
                    TripEndpoint::Building(b) => {
                        trips_to_bldg.insert(*b, idx);
                    }
                    TripEndpoint::Lane(l) => {
                        trips_to_border.insert(ui.primary.map.get_l(*l).dst_i, idx);
                    }
                },
                SpawnTrip::JustWalking(_, _, ref spot)
                | SpawnTrip::UsingTransit(_, _, ref spot, _, _, _) => match spot.connection {
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

        let mut bldg_colors = ObjectColorerBuilder::new(
            Text::prompt("buildings"),
            vec![
                ("1-2 cars needed", Color::BLUE),
                ("3-4 cars needed", Color::RED),
                (">= 5 cars needed", Color::BLACK),
            ],
        );
        let mut total_cars_needed = 0;
        for (b, count) in &scenario.individ_parked_cars {
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
            bldg_colors.add(ID::Building(*b), color);
        }

        let (filled_spots, free_parking_spots) = ui.primary.sim.get_all_parking_spots();
        assert!(filled_spots.is_empty());

        ScenarioManager {
            menu: ModalMenu::new(
                "Scenario Editor",
                vec![
                    (hotkey(Key::S), "save"),
                    (hotkey(Key::E), "edit"),
                    (hotkey(Key::R), "instantiate"),
                ],
                ctx,
            ),
            general_tools: MenuUnderButton::new(
                "assets/ui/hamburger.png",
                "General",
                vec![
                    (hotkey(Key::Escape), "quit"),
                    (hotkey(Key::F1), "take a screenshot"),
                ],
                0.2,
                ctx,
            ),
            common: CommonState::new(ctx),
            scenario,
            trips_from_bldg,
            trips_to_bldg,
            trips_from_border,
            trips_to_border,
            total_cars_needed,
            total_parking_spots: free_parking_spots.len(),
            bldg_colors: bldg_colors.build(ctx, &ui.primary.map),
            demand: None,
        }
    }
}

impl State for ScenarioManager {
    fn event(&mut self, ctx: &mut EventCtx, ui: &mut UI) -> Transition {
        // TODO Calculate this once? Except when we modify it, nice to automatically pick up
        // changes...
        {
            let mut txt = Text::new();
            txt.add(Line(&self.scenario.scenario_name));
            txt.add(Line(format!(
                "{} total trips",
                prettyprint_usize(self.scenario.individ_trips.len())
            )));
            txt.add(Line(format!(
                "seed {} parked cars",
                prettyprint_usize(self.total_cars_needed)
            )));
            txt.add(Line(format!(
                "{} parking spots",
                prettyprint_usize(self.total_parking_spots),
            )));
            self.menu.set_info(ctx, txt);
        }
        self.menu.event(ctx);
        self.general_tools.event(ctx);
        ctx.canvas.handle_event(ctx.input);
        if ctx.redo_mouseover() {
            ui.recalculate_current_selection(ctx);
        }
        if let Some(t) = self.common.event(ctx, ui) {
            return t;
        }

        if self.general_tools.action("quit") {
            return Transition::Pop;
        } else if self.general_tools.action("take a screenshot") {
            return Transition::KeepWithMode(EventLoopMode::ScreenCaptureCurrentShot);
        } else if self.menu.action("save") {
            self.scenario.save();
        } else if self.menu.action("edit") {
            return Transition::Push(Box::new(ScenarioEditor {
                scenario: self.scenario.clone(),
                wizard: Wizard::new(),
            }));
        } else if self.menu.action("instantiate") {
            return Transition::PopThenReplace(Box::new(SandboxMode::new(
                ctx,
                ui,
                GameplayMode::PlayScenario(self.scenario.scenario_name.clone()),
            )));
        }

        if self.demand.is_some() && self.menu.consume_action("stop showing paths", ctx) {
            self.demand = None;
        }

        if let Some(ID::Building(b)) = ui.primary.current_selection {
            let from = self.trips_from_bldg.get(b);
            let to = self.trips_to_bldg.get(b);
            if !from.is_empty() || !to.is_empty() {
                if ctx.input.contextual_action(Key::T, "browse trips") {
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
                    && ctx
                        .input
                        .contextual_action(Key::P, "show trips to and from")
                {
                    self.demand = Some(show_demand(&self.scenario, from, to, OD::Bldg(b), ui, ctx));
                    self.menu
                        .push_action(hotkey(Key::P), "stop showing paths", ctx);
                }
            }
        } else if let Some(ID::Intersection(i)) = ui.primary.current_selection {
            let from = self.trips_from_border.get(i);
            let to = self.trips_to_border.get(i);
            if !from.is_empty() || !to.is_empty() {
                if ctx.input.contextual_action(Key::T, "browse trips") {
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
                    && ctx
                        .input
                        .contextual_action(Key::P, "show trips to and from")
                {
                    self.demand = Some(show_demand(
                        &self.scenario,
                        from,
                        to,
                        OD::Border(i),
                        ui,
                        ctx,
                    ));
                    self.menu
                        .push_action(hotkey(Key::P), "stop showing paths", ctx);
                }
            }
        }

        Transition::Keep
    }

    fn draw_default_ui(&self) -> bool {
        false
    }

    fn draw(&self, g: &mut GfxCtx, ui: &UI) {
        // TODO Let common contribute draw_options...
        if let Some(ref p) = self.demand {
            ui.draw(
                g,
                self.common.draw_options(ui),
                &ui.primary.sim,
                &ShowEverything::new(),
            );
            g.redraw(p);
        // TODO Color legend!
        } else {
            self.bldg_colors.draw(g, ui);
        }

        self.menu.draw(g);
        self.general_tools.draw(g);
        self.common.draw_no_osd(g, ui);

        if let Some(ID::Building(b)) = ui.primary.current_selection {
            let mut osd = CommonState::default_osd(ID::Building(b), ui);
            osd.append(Line(format!(
                ". {} trips from here, {} trips to here, {} parked cars needed",
                self.trips_from_bldg.get(b).len(),
                self.trips_to_bldg.get(b).len(),
                self.scenario.individ_parked_cars[&b]
            )));
            CommonState::draw_custom_osd(g, osd);
        } else if let Some(ID::Intersection(i)) = ui.primary.current_selection {
            let mut osd = CommonState::default_osd(ID::Intersection(i), ui);
            osd.append(Line(format!(
                ". {} trips from here, {} trips to here",
                self.trips_from_border.get(i).len(),
                self.trips_to_border.get(i).len(),
            )));
            CommonState::draw_custom_osd(g, osd);
        } else {
            CommonState::draw_osd(g, ui, &ui.primary.current_selection);
        }
    }
}

struct ScenarioEditor {
    scenario: Scenario,
    wizard: Wizard,
}

impl State for ScenarioEditor {
    fn event(&mut self, ctx: &mut EventCtx, ui: &mut UI) -> Transition {
        if let Some(()) = edit_scenario(&ui.primary.map, &mut self.scenario, self.wizard.wrap(ctx))
        {
            // TODO autosave, or at least make it clear there are unsaved edits
            let scenario = self.scenario.clone();
            return Transition::PopWithData(Box::new(|state, _, _| {
                let mut manager = state.downcast_mut::<ScenarioManager>().unwrap();
                manager.scenario = scenario;
                // Don't need to update trips_from_bldg or trips_to_bldg, since edit_scenario
                // doesn't touch individ_trips.
            }));
        } else if self.wizard.aborted() {
            return Transition::Pop;
        }
        Transition::Keep
    }

    fn draw(&self, g: &mut GfxCtx, ui: &UI) {
        if let Some(neighborhood) = self.wizard.current_menu_choice::<Neighborhood>() {
            g.draw_polygon(ui.cs.get("neighborhood polygon"), &neighborhood.polygon);
        }
        self.wizard.draw(g);
    }
}

fn edit_scenario(map: &Map, scenario: &mut Scenario, mut wizard: WrappedWizard) -> Option<()> {
    let seed_parked = "Seed parked cars";
    let spawn = "Spawn agents";
    let spawn_border = "Spawn agents from a border";
    let randomize = "Randomly spawn stuff from/to every neighborhood";
    match wizard
        .choose_string("What kind of edit?", || {
            vec![seed_parked, spawn, spawn_border, randomize]
        })?
        .as_str()
    {
        x if x == seed_parked => {
            scenario.seed_parked_cars.push(SeedParkedCars {
                neighborhood: choose_neighborhood(
                    map,
                    &mut wizard,
                    "Seed parked cars in what area?",
                )?,
                cars_per_building: input_weighted_usize(
                    &mut wizard,
                    "How many cars per building? (ex: 4,4,2)",
                )?,
            });
        }
        x if x == spawn => {
            let (start_time, stop_time) =
                pick_time_range(&mut wizard, "Start spawning when?", "Stop spawning when?")?;
            scenario.spawn_over_time.push(SpawnOverTime {
                num_agents: wizard.input_usize("Spawn how many agents?")?,
                start_time,
                stop_time,
                start_from_neighborhood: choose_neighborhood(
                    map,
                    &mut wizard,
                    "Where should the agents start?",
                )?,
                goal: choose_origin_destination(map, &mut wizard, "Where should the agents go?")?,
                percent_biking: wizard
                    .input_percent("What percent of the walking trips will bike instead?")?,
                percent_use_transit: wizard.input_percent(
                    "What percent of the walking trips will consider taking transit?",
                )?,
            });
        }
        x if x == spawn_border => {
            let (start_time, stop_time) =
                pick_time_range(&mut wizard, "Start spawning when?", "Stop spawning when?")?;
            scenario.border_spawn_over_time.push(BorderSpawnOverTime {
                num_peds: wizard.input_usize("Spawn how many pedestrians?")?,
                num_cars: wizard.input_usize("Spawn how many cars?")?,
                num_bikes: wizard.input_usize("Spawn how many bikes?")?,
                start_time,
                stop_time,
                // TODO validate it's a border!
                start_from_border: choose_intersection(
                    &mut wizard,
                    "Which border should the agents spawn at?",
                )?,
                goal: choose_origin_destination(map, &mut wizard, "Where should the agents go?")?,
                percent_use_transit: wizard.input_percent(
                    "What percent of the walking trips will consider taking transit?",
                )?,
            });
        }
        x if x == randomize => {
            let neighborhoods = Neighborhood::load_all(map.get_name(), &map.get_gps_bounds());
            for (src, _) in &neighborhoods {
                for (dst, _) in &neighborhoods {
                    scenario.spawn_over_time.push(SpawnOverTime {
                        num_agents: 100,
                        start_time: Duration::ZERO,
                        stop_time: Duration::minutes(10),
                        start_from_neighborhood: src.to_string(),
                        goal: OriginDestination::Neighborhood(dst.to_string()),
                        percent_biking: 0.1,
                        percent_use_transit: 0.2,
                    });
                }
            }
        }
        _ => unreachable!(),
    };
    Some(())
}

fn choose_neighborhood(map: &Map, wizard: &mut WrappedWizard, query: &str) -> Option<String> {
    // Load the full object, since we usually visualize the neighborhood when menuing over it
    wizard
        .choose(query, || {
            Choice::from(Neighborhood::load_all(map.get_name(), map.get_gps_bounds()))
        })
        .map(|(n, _)| n)
}

fn input_weighted_usize(wizard: &mut WrappedWizard, query: &str) -> Option<WeightedUsizeChoice> {
    wizard.input_something(
        query,
        None,
        Box::new(|line| WeightedUsizeChoice::parse(&line)),
    )
}

// TODO Validate the intersection exists? Let them pick it with the cursor?
fn choose_intersection(wizard: &mut WrappedWizard, query: &str) -> Option<IntersectionID> {
    wizard.input_something(
        query,
        None,
        Box::new(|line| usize::from_str_radix(&line, 10).ok().map(IntersectionID)),
    )
}

fn choose_origin_destination(
    map: &Map,
    wizard: &mut WrappedWizard,
    query: &str,
) -> Option<OriginDestination> {
    let neighborhood = "Neighborhood";
    let border = "Border intersection";
    if wizard.choose_string(query, || vec![neighborhood, border])? == neighborhood {
        choose_neighborhood(map, wizard, query).map(OriginDestination::Neighborhood)
    } else {
        choose_intersection(wizard, query).map(OriginDestination::Border)
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
    WizardState::new(Box::new(move |wiz, ctx, ui| {
        let warp_to = wiz
            .wrap(ctx)
            .choose(&format!("Trips from/to this {}", noun), || {
                // TODO Panics if there are two duplicate trips (b1124 in montlake)
                indices
                    .iter()
                    .map(|idx| {
                        let trip = &scenario.individ_trips[*idx];
                        Choice::new(
                            describe(trip, home, &ui.primary.map),
                            other_endpt(trip, home, &ui.primary.map),
                        )
                    })
                    .collect()
            })?
            .1;
        Some(Transition::ReplaceWithMode(
            Warping::new(
                ctx,
                warp_to.canonical_point(&ui.primary).unwrap(),
                None,
                Some(warp_to),
                &mut ui.primary,
            ),
            EventLoopMode::Animation,
        ))
    }))
}

fn describe(trip: &SpawnTrip, home: OD, map: &Map) -> String {
    let driving_goal = |goal: &TripEndpoint| match goal {
        TripEndpoint::Building(b) => {
            if OD::Bldg(*b) == home {
                "HERE".to_string()
            } else {
                b.to_string()
            }
        }
        TripEndpoint::Lane(l) => {
            let i = map.get_l(*l).dst_i;
            if OD::Border(i) == home {
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

    match trip {
        SpawnTrip::CarAppearing {
            depart,
            start,
            goal,
            is_bike,
        } => format!(
            "{}: {} appears at {}, goes to {}",
            depart,
            if *is_bike { "bike" } else { "car" },
            start.lane(),
            driving_goal(goal)
        ),
        SpawnTrip::MaybeUsingParkedCar(depart, start_bldg, goal) => format!(
            "{}: try to drive from {} to {}",
            depart,
            if OD::Bldg(*start_bldg) == home {
                "HERE".to_string()
            } else {
                start_bldg.to_string()
            },
            driving_goal(goal),
        ),
        SpawnTrip::UsingBike(depart, start, goal) => format!(
            "{}: bike from {} to {}",
            depart,
            sidewalk_spot(start),
            driving_goal(goal)
        ),
        SpawnTrip::JustWalking(depart, start, goal) => format!(
            "{}: walk from {} to {}",
            depart,
            sidewalk_spot(start),
            sidewalk_spot(goal)
        ),
        SpawnTrip::UsingTransit(depart, start, goal, route, _, _) => format!(
            "{}: bus from {} to {} using {}",
            depart,
            sidewalk_spot(start),
            sidewalk_spot(goal),
            route
        ),
    }
}

fn other_endpt(trip: &SpawnTrip, home: OD, map: &Map) -> ID {
    let driving_goal = |goal: &TripEndpoint| match goal {
        TripEndpoint::Building(b) => ID::Building(*b),
        TripEndpoint::Lane(l) => ID::Intersection(map.get_l(*l).dst_i),
    };
    let sidewalk_spot = |spot: &SidewalkSpot| match &spot.connection {
        SidewalkPOI::Building(b) => ID::Building(*b),
        SidewalkPOI::Border(i) => ID::Intersection(*i),
        x => panic!("other_endpt for {:?}?", x),
    };

    let (from, to) = match trip {
        SpawnTrip::CarAppearing { start, goal, .. } => (
            ID::Intersection(map.get_l(start.lane()).src_i),
            driving_goal(goal),
        ),
        SpawnTrip::MaybeUsingParkedCar(_, start_bldg, goal) => {
            (ID::Building(*start_bldg), driving_goal(goal))
        }
        SpawnTrip::UsingBike(_, start, goal) => (sidewalk_spot(start), driving_goal(goal)),
        SpawnTrip::JustWalking(_, start, goal) => (sidewalk_spot(start), sidewalk_spot(goal)),
        SpawnTrip::UsingTransit(_, start, goal, _, _, _) => {
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
    ui: &UI,
    ctx: &EventCtx,
) -> Drawable {
    let mut from_ids = Counter::new();
    for idx in from {
        from_ids.inc(other_endpt(
            &scenario.individ_trips[*idx],
            home,
            &ui.primary.map,
        ));
    }
    let mut to_ids = Counter::new();
    for idx in to {
        to_ids.inc(other_endpt(
            &scenario.individ_trips[*idx],
            home,
            &ui.primary.map,
        ));
    }
    let from_count = from_ids.consume();
    let mut to_count = to_ids.consume();
    let max_count =
        (*from_count.values().max().unwrap()).max(*to_count.values().max().unwrap()) as f64;

    let mut batch = GeomBatch::new();
    let home_pt = match home {
        OD::Bldg(b) => ui.primary.map.get_b(b).polygon.center(),
        OD::Border(i) => ui.primary.map.get_i(i).polygon.center(),
    };

    for (id, cnt) in from_count {
        // Bidirectional?
        if let Some(other_cnt) = to_count.remove(&id) {
            let width = Distance::meters(1.0)
                + ((cnt.max(other_cnt) as f64) / max_count) * Distance::meters(2.0);
            batch.push(
                Color::PURPLE.alpha(0.8),
                PolyLine::new(vec![home_pt, id.canonical_point(&ui.primary).unwrap()])
                    .make_polygons(width),
            );
        } else {
            let width = Distance::meters(1.0) + ((cnt as f64) / max_count) * Distance::meters(2.0);
            batch.push(
                Color::RED.alpha(0.8),
                PolyLine::new(vec![home_pt, id.canonical_point(&ui.primary).unwrap()])
                    .make_arrow(width)
                    .unwrap(),
            );
        }
    }
    for (id, cnt) in to_count {
        let width = Distance::meters(1.0) + ((cnt as f64) / max_count) * Distance::meters(2.0);
        batch.push(
            Color::BLUE.alpha(0.8),
            PolyLine::new(vec![id.canonical_point(&ui.primary).unwrap(), home_pt])
                .make_arrow(width)
                .unwrap(),
        );
    }

    ctx.prerender.upload(batch)
}
