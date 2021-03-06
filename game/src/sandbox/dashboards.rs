use crate::app::App;
use crate::colors;
use crate::common::ShowBusRoute;
use crate::game::{State, Transition};
use crate::helpers::ID;
use crate::helpers::{cmp_count_fewer, cmp_count_more, cmp_duration_shorter};
use crate::managed::{Callback, ManagedGUIState, WrappedComposite};
use crate::sandbox::SandboxMode;
use abstutil::prettyprint_usize;
use abstutil::Counter;
use ezgui::{
    hotkey, Button, Color, Composite, EventCtx, Histogram, Key, Line, ManagedWidget, Plot,
    PlotOptions, Series, Text,
};
use geom::{Duration, Statistic, Time};
use map_model::BusRouteID;
use sim::{TripID, TripMode};
use std::collections::BTreeMap;

#[derive(PartialEq, Clone, Copy)]
pub enum Tab {
    TripsSummary,
    IndividualFinishedTrips(Option<TripMode>),
    ParkingOverhead,
    ExploreBusRoute,
}

// Oh the dashboards melted, but we still had the radio
pub fn make(ctx: &mut EventCtx, app: &App, tab: Tab) -> Box<dyn State> {
    let tab_data = vec![
        (Tab::TripsSummary, "Trips summary"),
        (
            Tab::IndividualFinishedTrips(None),
            "Individual finished trips",
        ),
        (Tab::ParkingOverhead, "Parking overhead analysis"),
        (Tab::ExploreBusRoute, "Explore a bus route"),
    ];

    let tabs = tab_data
        .iter()
        .map(|(t, label)| {
            if *t == tab {
                Button::inactive_selected_button(ctx, *label)
            } else {
                WrappedComposite::text_button(ctx, label, None)
            }
            .margin(5)
        })
        .collect::<Vec<_>>();

    let (content, cbs) = match tab {
        Tab::TripsSummary => (trips_summary_prebaked(ctx, app), Vec::new()),
        Tab::IndividualFinishedTrips(None) => pick_finished_trips_mode(ctx),
        Tab::IndividualFinishedTrips(Some(m)) => pick_finished_trips(m, ctx, app),
        Tab::ParkingOverhead => (parking_overhead(ctx, app), Vec::new()),
        Tab::ExploreBusRoute => pick_bus_route(ctx, app),
    };

    let mut c = WrappedComposite::new(
        Composite::new(ManagedWidget::col(vec![
            WrappedComposite::svg_button(
                ctx,
                "../data/system/assets/pregame/back.svg",
                "back",
                hotkey(Key::Escape),
            )
            .align_left(),
            ManagedWidget::row(tabs).bg(colors::PANEL_BG),
            content.bg(colors::PANEL_BG),
        ]))
        // TODO Want to use exact, but then scrolling breaks. exact_size_percent will fix the
        // jumpiness though.
        .max_size_percent(90, 80)
        .build(ctx),
    )
    .cb("back", Box::new(|_, _| Some(Transition::Pop)));
    for (t, label) in tab_data {
        // TODO Not quite... all the IndividualFinishedTrips variants need to act the same
        if t != tab {
            c = c.cb(
                label,
                Box::new(move |ctx, app| Some(Transition::Replace(make(ctx, app, t)))),
            );
        }
    }
    for (name, cb) in cbs {
        c = c.cb(&name, cb);
    }

    ManagedGUIState::fullscreen(c)
}

fn trips_summary_prebaked(ctx: &EventCtx, app: &App) -> ManagedWidget {
    if app.has_prebaked().is_none() {
        return trips_summary_not_prebaked(ctx, app);
    }

    let (now_all, now_aborted, now_per_mode) = app
        .primary
        .sim
        .get_analytics()
        .trip_times(app.primary.sim.time());
    let (baseline_all, baseline_aborted, baseline_per_mode) =
        app.prebaked().trip_times(app.primary.sim.time());

    // TODO Include unfinished count
    let mut txt = Text::new();
    txt.add_appended(vec![
        Line("Trips as of "),
        Line(app.primary.sim.time().ampm_tostring()).roboto_bold(),
    ]);
    txt.highlight_last_line(Color::BLUE);
    txt.add_appended(vec![
        Line(format!(
            "{} aborted trips (",
            prettyprint_usize(now_aborted)
        )),
        cmp_count_fewer(now_aborted, baseline_aborted),
        Line(")"),
    ]);
    // TODO Refactor
    txt.add_appended(vec![
        Line(format!(
            "{} total trips (",
            prettyprint_usize(now_all.count())
        )),
        cmp_count_more(now_all.count(), baseline_all.count()),
        Line(")"),
    ]);
    if now_all.count() > 0 && baseline_all.count() > 0 {
        for stat in Statistic::all() {
            // TODO Ideally we could indent
            txt.add(Line(format!("{}: {} (", stat, now_all.select(stat))));
            txt.append_all(cmp_duration_shorter(
                now_all.select(stat),
                baseline_all.select(stat),
            ));
            txt.append(Line(")"));
        }
    }

    for mode in TripMode::all() {
        let a = &now_per_mode[&mode];
        let b = &baseline_per_mode[&mode];
        txt.add_appended(vec![
            Line(format!("{} {} trips (", prettyprint_usize(a.count()), mode)),
            cmp_count_more(a.count(), b.count()),
            Line(")"),
        ]);
        txt.highlight_last_line(Color::BLUE);
        if a.count() > 0 && b.count() > 0 {
            for stat in Statistic::all() {
                txt.add(Line(format!("{}: {} (", stat, a.select(stat))));
                txt.append_all(cmp_duration_shorter(a.select(stat), b.select(stat)));
                txt.append(Line(")"));
            }
        }
    }

    ManagedWidget::col(vec![
        ManagedWidget::draw_text(ctx, txt),
        finished_trips_plot(ctx, app).bg(colors::SECTION_BG),
        ManagedWidget::draw_text(
            ctx,
            Text::from(Line("Are trips faster or slower than the baseline?")),
        ),
        Histogram::new(
            app.primary
                .sim
                .get_analytics()
                .trip_time_deltas(app.primary.sim.time(), app.prebaked()),
            ctx,
        )
        .bg(colors::SECTION_BG),
        ManagedWidget::draw_text(ctx, Text::from(Line("Active agents").roboto_bold())),
        Plot::new_usize(
            ctx,
            vec![
                Series {
                    label: "Baseline".to_string(),
                    color: Color::BLUE.alpha(0.5),
                    pts: app.prebaked().active_agents(Time::END_OF_DAY),
                },
                Series {
                    label: "Current simulation".to_string(),
                    color: Color::RED,
                    pts: app
                        .primary
                        .sim
                        .get_analytics()
                        .active_agents(app.primary.sim.time()),
                },
            ],
            PlotOptions::new(),
        ),
    ])
}

fn trips_summary_not_prebaked(ctx: &EventCtx, app: &App) -> ManagedWidget {
    let (all, aborted, per_mode) = app
        .primary
        .sim
        .get_analytics()
        .trip_times(app.primary.sim.time());

    // TODO Include unfinished count
    let mut txt = Text::new();
    txt.add_appended(vec![
        Line("Trips as of "),
        Line(app.primary.sim.time().ampm_tostring()).roboto_bold(),
    ]);
    txt.highlight_last_line(Color::BLUE);
    txt.add(Line(format!(
        "{} aborted trips",
        prettyprint_usize(aborted)
    )));
    txt.add(Line(format!(
        "{} total trips",
        prettyprint_usize(all.count())
    )));
    if all.count() > 0 {
        for stat in Statistic::all() {
            txt.add(Line(format!("{}: {}", stat, all.select(stat))));
        }
    }

    for mode in TripMode::all() {
        let a = &per_mode[&mode];
        txt.add(Line(format!(
            "{} {} trips",
            prettyprint_usize(a.count()),
            mode
        )));
        txt.highlight_last_line(Color::BLUE);
        if a.count() > 0 {
            for stat in Statistic::all() {
                txt.add(Line(format!("{}: {}", stat, a.select(stat))));
            }
        }
    }

    ManagedWidget::col(vec![
        ManagedWidget::draw_text(ctx, txt),
        finished_trips_plot(ctx, app).bg(colors::SECTION_BG),
        ManagedWidget::draw_text(ctx, Text::from(Line("Active agents").roboto_bold())),
        Plot::new_usize(
            ctx,
            vec![Series {
                label: "Active agents".to_string(),
                color: Color::RED,
                pts: app
                    .primary
                    .sim
                    .get_analytics()
                    .active_agents(app.primary.sim.time()),
            }],
            PlotOptions::new(),
        ),
    ])
}

fn finished_trips_plot(ctx: &EventCtx, app: &App) -> ManagedWidget {
    let mut lines: Vec<(String, Color, Option<TripMode>)> = TripMode::all()
        .into_iter()
        .map(|m| (m.to_string(), color_for_mode(m, app), Some(m)))
        .collect();
    lines.push(("aborted".to_string(), Color::PURPLE.alpha(0.5), None));

    // What times do we use for interpolation?
    let num_x_pts = 100;
    let mut times = Vec::new();
    for i in 0..num_x_pts {
        let percent_x = (i as f64) / ((num_x_pts - 1) as f64);
        let t = app.primary.sim.time().percent_of(percent_x);
        times.push(t);
    }

    // Gather the data
    let mut counts = Counter::new();
    let mut pts_per_mode: BTreeMap<Option<TripMode>, Vec<(Time, usize)>> =
        lines.iter().map(|(_, _, m)| (*m, Vec::new())).collect();
    for (t, _, m, _) in &app.primary.sim.get_analytics().finished_trips {
        counts.inc(*m);
        if *t > times[0] {
            times.remove(0);
            for (_, _, mode) in &lines {
                pts_per_mode
                    .get_mut(mode)
                    .unwrap()
                    .push((*t, counts.get(*mode)));
            }
        }
    }
    // Don't forget the last batch
    for (_, _, mode) in &lines {
        pts_per_mode
            .get_mut(mode)
            .unwrap()
            .push((app.primary.sim.time(), counts.get(*mode)));
    }

    let plot = Plot::new_usize(
        ctx,
        lines
            .into_iter()
            .map(|(label, color, m)| Series {
                label,
                color,
                pts: pts_per_mode.remove(&m).unwrap(),
            })
            .collect(),
        PlotOptions::new(),
    );
    ManagedWidget::col(vec![
        ManagedWidget::draw_text(ctx, Text::from(Line("finished trips"))),
        plot.margin(10),
    ])
}

fn pick_finished_trips_mode(ctx: &EventCtx) -> (ManagedWidget, Vec<(String, Callback)>) {
    let mut buttons = Vec::new();
    let mut cbs: Vec<(String, Callback)> = Vec::new();

    for mode in TripMode::all() {
        buttons.push(WrappedComposite::text_button(ctx, &mode.to_string(), None));
        cbs.push((
            mode.to_string(),
            Box::new(move |ctx, app| {
                Some(Transition::Replace(make(
                    ctx,
                    app,
                    Tab::IndividualFinishedTrips(Some(mode)),
                )))
            }),
        ));
    }

    (ManagedWidget::row(buttons).flex_wrap(ctx, 80), cbs)
}

fn pick_finished_trips(
    mode: TripMode,
    ctx: &EventCtx,
    app: &App,
) -> (ManagedWidget, Vec<(String, Callback)>) {
    let mut buttons = Vec::new();
    let mut cbs: Vec<(String, Callback)> = Vec::new();

    let mut filtered: Vec<&(Time, TripID, Option<TripMode>, Duration)> = app
        .primary
        .sim
        .get_analytics()
        .finished_trips
        .iter()
        .filter(|(_, _, m, _)| *m == Some(mode))
        .collect();
    filtered.sort_by_key(|(_, _, _, dt)| *dt);
    filtered.reverse();
    for (_, id, _, dt) in filtered {
        let label = format!("{} taking {}", id, dt);
        buttons.push(WrappedComposite::text_button(ctx, &label, None));
        let trip = *id;
        cbs.push((
            label,
            Box::new(move |_, _| {
                Some(Transition::PopWithData(Box::new(move |state, app, ctx| {
                    state
                        .downcast_mut::<SandboxMode>()
                        .unwrap()
                        .controls
                        .common
                        .as_mut()
                        .unwrap()
                        .launch_info_panel(ID::Trip(trip), ctx, app);
                })))
            }),
        ));
    }

    // TODO Indicate the current mode
    let (mode_picker, more_cbs) = pick_finished_trips_mode(ctx);
    cbs.extend(more_cbs);

    (
        ManagedWidget::col(vec![
            mode_picker,
            ManagedWidget::row(buttons).flex_wrap(ctx, 80),
        ]),
        cbs,
    )
}

fn parking_overhead(ctx: &EventCtx, app: &App) -> ManagedWidget {
    let mut txt = Text::new();
    for line in app.primary.sim.get_analytics().analyze_parking_phases() {
        txt.add_wrapped(line, 0.9 * ctx.canvas.window_width);
    }
    ManagedWidget::draw_text(ctx, txt)
}

fn pick_bus_route(ctx: &EventCtx, app: &App) -> (ManagedWidget, Vec<(String, Callback)>) {
    let mut buttons = Vec::new();
    let mut cbs: Vec<(String, Callback)> = Vec::new();

    let mut routes: Vec<(&String, BusRouteID)> = app
        .primary
        .map
        .get_all_bus_routes()
        .iter()
        .map(|r| (&r.name, r.id))
        .collect();
    // TODO Sort first by length, then lexicographically
    routes.sort_by_key(|(name, _)| name.to_string());

    for (name, id) in routes {
        buttons.push(WrappedComposite::text_button(ctx, name, None));
        cbs.push((
            name.to_string(),
            Box::new(move |_, _| {
                Some(Transition::Push(ShowBusRoute::make_route_picker(
                    vec![id],
                    false,
                )))
            }),
        ));
    }

    (ManagedWidget::row(buttons).flex_wrap(ctx, 80), cbs)
}

// TODO Refactor
fn color_for_mode(m: TripMode, app: &App) -> Color {
    match m {
        TripMode::Walk => app.cs.get("unzoomed pedestrian"),
        TripMode::Bike => app.cs.get("unzoomed bike"),
        TripMode::Transit => app.cs.get("unzoomed bus"),
        TripMode::Drive => app.cs.get("unzoomed car"),
    }
}
