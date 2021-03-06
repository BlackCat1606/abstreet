use crate::app::App;
use crate::common::Warping;
use crate::game::{State, Transition};
use crate::helpers::ID;
use ezgui::{Autocomplete, EventCtx, GfxCtx, InputResult};
use map_model::RoadID;
use std::collections::HashSet;

pub struct Navigator {
    autocomplete: Autocomplete<RoadID>,
}

impl Navigator {
    pub fn new(app: &App) -> Navigator {
        // TODO Canonicalize names, handling abbreviations like east/e and street/st
        Navigator {
            autocomplete: Autocomplete::new(
                "Warp where?",
                app.primary
                    .map
                    .all_roads()
                    .iter()
                    .map(|r| (r.get_name(), r.id))
                    .collect(),
            ),
        }
    }
}

impl State for Navigator {
    fn event(&mut self, ctx: &mut EventCtx, app: &mut App) -> Transition {
        let map = &app.primary.map;
        match self.autocomplete.event(ctx) {
            InputResult::Canceled => Transition::Pop,
            InputResult::Done(name, ids) => {
                // Roads share intersections, so of course there'll be overlap here.
                let mut cross_streets = HashSet::new();
                for r in &ids {
                    let road = map.get_r(*r);
                    for i in &[road.src_i, road.dst_i] {
                        for cross in &map.get_i(*i).roads {
                            if !ids.contains(cross) {
                                cross_streets.insert(*cross);
                            }
                        }
                    }
                }
                Transition::Replace(Box::new(CrossStreet {
                    first: *ids.iter().next().unwrap(),
                    autocomplete: Autocomplete::new(
                        &format!("{} and what?", name),
                        cross_streets
                            .into_iter()
                            .map(|r| (map.get_r(r).get_name(), r))
                            .collect(),
                    ),
                }))
            }
            InputResult::StillActive => Transition::Keep,
        }
    }

    fn draw(&self, g: &mut GfxCtx, _: &App) {
        self.autocomplete.draw(g);
    }
}

struct CrossStreet {
    first: RoadID,
    autocomplete: Autocomplete<RoadID>,
}

impl State for CrossStreet {
    // When None, this is done.
    fn event(&mut self, ctx: &mut EventCtx, app: &mut App) -> Transition {
        let map = &app.primary.map;
        match self.autocomplete.event(ctx) {
            InputResult::Canceled => {
                // Just warp to somewhere on the first road
                let road = map.get_r(self.first);
                println!("Warping to {}", road.get_name());
                Transition::Replace(Warping::new(
                    ctx,
                    road.center_pts.dist_along(road.center_pts.length() / 2.0).0,
                    None,
                    Some(ID::Lane(road.all_lanes()[0])),
                    &mut app.primary,
                ))
            }
            InputResult::Done(name, ids) => {
                println!(
                    "Warping to {} and {}",
                    map.get_r(self.first).get_name(),
                    name
                );
                let road = map.get_r(*ids.iter().next().unwrap());
                let pt = if map.get_i(road.src_i).roads.contains(&self.first) {
                    map.get_i(road.src_i).polygon.center()
                } else {
                    map.get_i(road.dst_i).polygon.center()
                };
                Transition::Replace(Warping::new(
                    ctx,
                    pt,
                    None,
                    Some(ID::Lane(road.all_lanes()[0])),
                    &mut app.primary,
                ))
            }
            InputResult::StillActive => Transition::Keep,
        }
    }

    fn draw(&self, g: &mut GfxCtx, _: &App) {
        self.autocomplete.draw(g);
    }
}
