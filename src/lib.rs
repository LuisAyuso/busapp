use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use dfui::taffy::{AlignItems, JustifyContent};
use dfui::widgets::{
    Container, Overlays, Shown, Snapshot, Styled, button, column, container, drawer,
    future_builder, on_back, overlays, row, scroll, spacer, stateful, stream_builder, text,
    text_input,
};
use dfui::{App, Color, Element, Font, Handle};

mod lines;
mod map;
mod network;
mod plan;
use futures_channel::{mpsc, oneshot};
use serde::Deserialize;
use serde::de::DeserializeOwned;

const API: &str = "https://smartinfo.ivb.at/api/JSON";
const BG: Color = Color::hex(0xf2f3f7);
const INK: Color = Color::hex(0x1d2030);
const MUTED: Color = Color::hex(0x6b7080);
const ACCENT: Color = Color::hex(0xc8102e);
const REFRESH: Duration = Duration::from_secs(20);
const RETRY: Duration = Duration::from_secs(5);
const MAX_ROWS: usize = 40;

#[derive(Deserialize, Clone, Default)]
pub struct Stop {
    pub uid: String,
    pub name: String,
}

#[derive(Deserialize)]
struct StopEntry {
    stop: Stop,
}

#[derive(Deserialize, Clone)]
pub struct Passage {
    pub route: String,
    pub direction: String,
    pub time: String,
    pub stat: String,
    pub plabel: String,
}

#[derive(Deserialize)]
struct RouteEntry {
    route: Route,
}

#[derive(Deserialize)]
struct Route {
    name: String,
}

/// IVB's own lines first, in their official order, then regional ones as the API lists them.
pub fn sort_lines(mut lines: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    lines.retain(|l| seen.insert(l.clone()));
    lines.sort_by_key(|l| {
        LINE_COLORS
            .iter()
            .position(|(n, _)| n == l)
            .unwrap_or(LINE_COLORS.len())
    });
    lines
}

/// PASSAGE mixes departures with other objects (`linedirections`, `stopidname`).
#[derive(Deserialize)]
struct PassageEntry {
    smartinfo: Option<Passage>,
}

fn get_text(url: &str) -> Result<String, String> {
    ureq::get(url)
        .call()
        .and_then(|mut r| r.body_mut().read_to_string())
        .map_err(|e| e.to_string())
}

fn get<T: DeserializeOwned>(url: &str) -> Result<T, String> {
    serde_json::from_str(&get_text(url)?).map_err(|e| e.to_string())
}

pub fn parse_passages(json: &str) -> Result<Vec<Passage>, String> {
    let entries: Vec<PassageEntry> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(entries
        .into_iter()
        .filter_map(|e| e.smartinfo)
        .take(MAX_ROWS)
        .collect())
}

/// Calls `fetch` on a thread and streams each result; `again` picks the delay
/// before the next call, or stops. Dropping the receiver ends the thread.
fn poll<T: Send + 'static>(
    fetch: impl Fn() -> Result<T, String> + Send + 'static,
    again: impl Fn(&Result<T, String>) -> Option<Duration> + Send + 'static,
) -> mpsc::UnboundedReceiver<Result<T, String>> {
    let (tx, rx) = mpsc::unbounded();
    std::thread::spawn(move || {
        loop {
            let r = fetch();
            let wait = again(&r);
            if tx.unbounded_send(r).is_err() {
                return;
            }
            match wait {
                Some(d) => std::thread::sleep(d),
                None => return,
            }
        }
    });
    rx
}

/// Stream collector that keeps only the newest item.
pub struct Latest<T>(Option<T>);

impl<T> Default for Latest<T> {
    fn default() -> Self {
        Latest(None)
    }
}

impl<T> Extend<T> for Latest<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, it: I) {
        if let Some(v) = it.into_iter().last() {
            self.0 = Some(v);
        }
    }
}

pub struct Bus;

impl App for Bus {
    fn view(&self) -> Element {
        let stops = stream_builder(
            (),
            |_| {
                poll(
                    || {
                        let mut s: Vec<Stop> = get::<Vec<StopEntry>>(&format!("{API}/STOPS"))?
                            .into_iter()
                            .map(|e| e.stop)
                            .collect();
                        s.sort_by(|a, b| a.name.cmp(&b.name));
                        Ok(s)
                    },
                    |r| r.is_err().then_some(RETRY),
                )
            },
            |snap: &Snapshot<Latest<Result<Vec<Stop>, String>>>| -> Element {
                match &snap.data.0 {
                    Some(Ok(stops)) => screen(Rc::new(stops.clone())),
                    Some(Err(e)) => message(&format!(
                        "Haltestellen konnten nicht geladen werden, neuer Versuch…\n{e}"
                    )),
                    None => message("Haltestellen werden geladen…"),
                }
            },
        );
        container(stops.fill()).bg(BG).fill().into()
    }
}

struct Ui {
    query: String,
    stop: Option<Stop>,
    /// Picking the other end of a trip from or to `stop`.
    picking: Option<Pick>,
    to: Option<Stop>,
    screen: Screen,
    hidden: HashSet<String>,
    zoom: i32,
    line: Option<String>,
    /// Index into the line's directions.
    dir: usize,
    /// Stop the network view is centred on, by snapshot index.
    center: Option<usize>,
    pin: Option<map::Pin>,
    /// Where the map centres, set by an address search.
    focus: Option<dfui::path::Point>,
    /// The address search over the map is open.
    finding: bool,
    address: String,
}

/// Which end of the trip `Ui::stop` is while picking the other.
#[derive(Clone, Copy, PartialEq)]
enum Pick {
    From,
    To,
}

/// Lines the network view shows until toggled.
const SHOWN_LINES: &[&str] = &[
    "1", "2", "3", "5", "6", "A", "C", "F", "J", "K", "M", "R", "T", "W",
];

impl Default for Ui {
    fn default() -> Self {
        let hidden = network::network()
            .lines()
            .into_iter()
            .filter(|l| !SHOWN_LINES.contains(&l.as_str()))
            .collect();
        Ui {
            query: String::new(),
            stop: None,
            picking: None,
            to: None,
            screen: Screen::default(),
            hidden,
            zoom: 0,
            line: None,
            dir: 0,
            center: None,
            pin: None,
            focus: None,
            finding: false,
            address: String::new(),
        }
    }
}

/// What the burger menu switches between.
#[derive(Default, Clone, Copy, PartialEq)]
enum Screen {
    #[default]
    Stops,
    Network,
    Lines,
    Map,
}

fn screen(stops: Rc<Vec<Stop>>) -> Element {
    overlays(move |ov| screen_in(stops.clone(), ov))
        .fill()
        .into()
}

fn screen_in(stops: Rc<Vec<Stop>>, ov: Handle<Overlays>) -> Element {
    stateful(move |ui: &Ui, h| -> Element {
        match (&ui.stop, &ui.to) {
            (Some(a), Some(b)) => page(
                h,
                &format!("{} → {}", a.name, b.name),
                Some(|u| u.to = None),
                [connections(a, b)],
            ),
            (Some(a), None) if ui.picking.is_some() => page(
                h,
                &match ui.picking {
                    Some(Pick::From) => format!("Von {} nach…", a.name),
                    _ => format!("Nach {} von…", a.name),
                },
                Some(|u| u.picking = None),
                search(&stops, &ui.query, h, |u, s| match u.picking.take() {
                    Some(Pick::To) => u.to = u.stop.replace(s),
                    _ => u.to = Some(s),
                }),
            ),
            (Some(a), None) => page(
                h,
                &a.name,
                Some(|u| u.stop = None),
                [
                    row([
                        plan(h, "Fahrt von hier", Pick::From),
                        plan(h, "Fahrt hierher", Pick::To),
                    ])
                    .gap(8.0)
                    .into(),
                    departures(a.uid.clone()),
                ],
            ),
            (None, _) => match (ui.screen, &ui.line) {
                (Screen::Network, _) => {
                    let view = network::NetworkView {
                        hidden: ui.hidden.clone(),
                        zoom: ui.zoom,
                        center: ui.center,
                        on_select: Box::new(move |cx, c| {
                            cx.update(h, move |u: &mut Ui| u.center = c)
                        }),
                    };
                    let mut body = vec![line_filters(h, &ui.hidden), view.into()];
                    body.extend(
                        ui.center
                            .and_then(|c| open_stop(h, &stops, c, "Abfahrten", ACCENT)),
                    );
                    page(h, "Liniennetz", Some(|u| u.screen = Screen::Stops), body)
                }
                (Screen::Lines, Some(l)) => page(
                    h,
                    &format!("Linie {l}"),
                    Some(|u| u.line = None),
                    lines::line(h, &stops, l, ui.dir),
                ),
                (Screen::Map, _) if ui.finding => map::find(h, &ui.address),
                (Screen::Map, _) => map::page(h, &stops, ui),
                (Screen::Lines, None) => page(
                    h,
                    "Linien",
                    Some(|u| u.screen = Screen::Stops),
                    [lines::all(h)],
                ),
                (Screen::Stops, _) => {
                    let mut body = vec![menu_button(h, ov)];
                    body.extend(search(&stops, &ui.query, h, |u, s| u.stop = Some(s)));
                    page(h, "Innsbruck Bus", None, body)
                }
            },
        }
    })
    .fill()
    .into()
}

/// A screen: title, a back button and the system back gesture when `back` is set, then `body`.
fn page(
    h: Handle<Ui>,
    title: &str,
    back: Option<fn(&mut Ui)>,
    body: impl IntoIterator<Item = Element>,
) -> Element {
    let title = text(title)
        .font(Font::BOLD)
        .size(24.0)
        .color(INK)
        .shrink(1.0);
    let Some(back) = back else {
        // The menu button, if any, leads `body` and joins the title's row.
        let mut body = body.into_iter();
        let lead = body.next().into_iter();
        let head = row(lead.chain([title.size(26.0).into()]))
            .align(AlignItems::CENTER)
            .gap(8.0);
        let mut items: Vec<Element> = vec![head.into()];
        items.extend(body);
        return column(items).gap(12.0).padding(16.0).fill().into();
    };
    page_with(h, title.into(), back, body)
}

/// A page with a back button before `title`, which may hold more than text.
fn page_with(
    h: Handle<Ui>,
    title: Element,
    back: fn(&mut Ui),
    body: impl IntoIterator<Item = Element>,
) -> Element {
    let mut items: Vec<Element> = vec![
        on_back(move |cx| cx.update(h, back)).into(),
        row([
            button(text("‹").size(24.0).color(INK))
                .flat()
                .color(BG)
                .on_click(move |cx| cx.update(h, back))
                .into(),
            title,
        ])
        .align(AlignItems::CENTER)
        .gap(8.0)
        .into(),
    ];
    items.extend(body);
    column(items).gap(12.0).padding(16.0).fill().into()
}

/// A button that starts picking the other end of a trip with the open stop.
fn plan(h: Handle<Ui>, label: &str, pick: Pick) -> Element {
    button(text(label).color(Color::WHITE))
        .color(ACCENT)
        .grow(1.0)
        .on_click(move |cx| {
            cx.update(h, move |u: &mut Ui| {
                u.picking = Some(pick);
                u.query.clear();
            })
        })
        .into()
}

fn search(stops: &[Stop], query: &str, h: Handle<Ui>, pick: fn(&mut Ui, Stop)) -> [Element; 2] {
    let q = query.to_lowercase();
    let hits = stops
        .iter()
        .filter(|s| s.uid.starts_with(&q) || s.name.to_lowercase().contains(&q))
        .take(MAX_ROWS)
        .map(|s| {
            let stop = s.clone();
            let name = column([
                row([
                    text(&s.name).size(17.0).color(INK).into(),
                    text(&s.uid).size(13.0).color(MUTED).into(),
                ])
                .align(AlignItems::BASELINE)
                .gap(8.0)
                .into(),
                stop_lines(&s.uid),
            ])
            .gap(6.0);
            button(name)
                .flat()
                .color(Color::WHITE)
                .justify(JustifyContent::START)
                .on_click(move |cx| {
                    let stop = stop.clone();
                    cx.update(h, move |u: &mut Ui| pick(u, stop))
                })
                .into()
        });
    [
        text_input(query)
            .placeholder("Haltestelle oder Nummer suchen")
            .accent(ACCENT)
            .on_change(move |cx, v| cx.update(h, move |u: &mut Ui| u.query = v))
            .into(),
        scroll(column(hits).gap(2.0)).grow(1.0).into(),
    ]
}

/// A button to the departures of snapshot stop `c`, if the stop list has it.
fn open_stop(h: Handle<Ui>, stops: &[Stop], c: usize, note: &str, color: Color) -> Option<Element> {
    let name = &network::network().stops[c].0;
    let stop = stops.iter().find(|s| &s.name == name)?.clone();
    let label = text(format!("{name} – {note} ›")).color(Color::WHITE);
    let open = button(label).color(color).on_click(move |cx| {
        let stop = stop.clone();
        cx.update(h, move |u: &mut Ui| u.stop = Some(stop))
    });
    Some(open.into())
}

fn menu_button(h: Handle<Ui>, ov: Handle<Overlays>) -> Element {
    let bar = || -> Element {
        container(spacer())
            .width(20.0)
            .height(2.5)
            .radius(1.0)
            .bg(INK)
            .into()
    };
    button(column([bar(), bar(), bar()]).gap(4.0))
        .flat()
        .color(BG)
        .on_click(move |cx| ov.show(cx, move |me| drawer(menu(h, me))))
        .into()
}

fn menu(h: Handle<Ui>, me: Shown) -> Element {
    let item = move |label: &str, to: Screen| -> Element {
        button(text(label).size(17.0).color(INK))
            .flat()
            .color(Color::WHITE)
            .justify(JustifyContent::START)
            .on_click(move |cx| {
                me.close(cx);
                cx.update(h, move |u: &mut Ui| {
                    u.screen = to;
                    u.line = None;
                })
            })
            .into()
    };
    column([
        text("Innsbruck Bus")
            .font(Font::BOLD)
            .size(20.0)
            .color(INK)
            .into(),
        item("Haltestellen", Screen::Stops),
        item("Liniennetz", Screen::Network),
        item("Linien", Screen::Lines),
        item("Karte", Screen::Map),
    ])
    .gap(8.0)
    .into()
}

/// A toggle per line in its color, greyed while hidden, then zoom buttons.
fn line_filters(h: Handle<Ui>, hidden: &HashSet<String>) -> Element {
    let toggles = network::network().lines().into_iter().map(|l| {
        let (bg, fg) = match hidden.contains(&l) {
            true => (Color::hex(0xdcdde3), MUTED),
            false => line_color(&l),
        };
        button(text(&l).font(Font::BOLD).size(13.0).color(fg))
            .color(bg)
            .flat()
            .radius(6.0)
            .padding_xy(8.0, 3.0)
            .on_click(move |cx| {
                let l = l.clone();
                cx.update(h, move |u: &mut Ui| {
                    if !u.hidden.remove(&l) {
                        u.hidden.insert(l);
                    }
                })
            })
            .into()
    });
    let zoom = |label: &str, by: i32| -> Element {
        button(text(label).size(18.0).color(INK))
            .flat()
            .color(Color::WHITE)
            .radius(6.0)
            .padding_xy(10.0, 0.0)
            .on_click(move |cx| cx.update(h, move |u: &mut Ui| u.zoom += by))
            .into()
    };
    row(toggles.chain([zoom("−", -1), zoom("+", 1)]))
        .wrap()
        .gap(4.0)
        .into()
}

/// Direct and one-change connections, refreshed like departures.
fn connections(a: &Stop, b: &Stop) -> Element {
    let ids = (a.uid.parse::<i64>(), b.uid.parse::<i64>());
    let (Ok(a), Ok(b)) = ids else {
        return message("Unbekannte Haltestelle.");
    };
    stream_builder(
        (a, b),
        move |_| {
            poll(
                move || plan::fetch(a, b),
                |r| Some(if r.is_ok() { REFRESH } else { RETRY }),
            )
        },
        |snap: &Snapshot<Latest<Result<plan::Plan, String>>>| -> Element {
            match &snap.data.0 {
                None => message("Verbindungen werden gesucht…"),
                Some(Err(e)) => message(&format!("Verbindungen konnten nicht geladen werden, neuer Versuch…\n{e}")),
                Some(Ok(p)) if p.connections.is_empty() => {
                    message("Bei den nächsten Abfahrten gibt es keine Verbindung mit höchstens zweimal Umsteigen.")
                }
                Some(Ok(p)) => {
                    scroll(column(p.connections.iter().map(|c| connection(p.now, c))).gap(6.0))
                        .grow(1.0)
                        .into()
                }
            }
        },
    )
    .grow(1.0)
    .into()
}

fn connection(now: Option<f64>, c: &plan::Connection) -> Element {
    let first = &c.legs[0];
    let mut lines: Vec<Element> = vec![];
    for (i, l) in c.legs.iter().enumerate() {
        if i > 0 {
            lines.push(text("›").size(18.0).color(MUTED).into());
        }
        lines.push(badge(&l.line, 15.0).padding_xy(7.0, 2.0).into());
    }
    lines.push(spacer().grow(1.0).into());
    lines.push(
        text(format!(
            "{} → {}{}",
            plan::clock(now, c.dep()),
            if c.estimated() { "~" } else { "" },
            plan::clock(now, c.arr())
        ))
        .font(Font::BOLD)
        .size(16.0)
        .color(INK)
        .into(),
    );
    let mut rows: Vec<Element> = vec![
        row(lines).align(AlignItems::CENTER).gap(6.0).into(),
        text(format!(
            "{}Steig {} · {} min Fahrt",
            if c.dep() < 60.0 {
                format!("Abfahrt in {} min · ", c.dep().round())
            } else {
                String::new()
            },
            first.platform,
            (c.arr() - c.dep()).round()
        ))
        .size(13.0)
        .color(MUTED)
        .into(),
    ];
    for w in c.legs.windows(2) {
        rows.push(
            text(format!(
                "Umsteigen in {}: {} min warten, {} Richtung {}",
                w[1].from,
                (w[1].dep - w[0].arr).round(),
                w[1].line,
                w[1].direction
            ))
            .size(13.0)
            .color(MUTED)
            .into(),
        );
    }
    column(rows)
        .gap(4.0)
        .padding_xy(12.0, 10.0)
        .bg(Color::WHITE)
        .radius(10.0)
        .into()
}

thread_local! {
    /// Lines per stop uid; they don't change while the app runs.
    static LINES: RefCell<HashMap<String, Rc<Vec<String>>>> = RefCell::default();
}

/// Badges of the lines serving a stop, fetched once per stop on first show.
// ponytail: a thread per visible row (≤ MAX_ROWS); a pool if that ever hurts.
fn stop_lines(uid: &str) -> Element {
    if let Some(lines) = LINES.with_borrow(|m| m.get(uid).cloned()) {
        return badges(&lines);
    }
    let id = uid.to_string();
    future_builder(
        uid,
        move |_| {
            let (tx, rx) = oneshot::channel();
            let url = format!("{API}/ROUTES?stopID={id}");
            std::thread::spawn(move || {
                let _ = tx.send(get::<Vec<RouteEntry>>(&url));
            });
            async move {
                let routes = rx.await.ok()?.ok()?;
                let lines = Rc::new(sort_lines(
                    routes.into_iter().map(|r| r.route.name).collect(),
                ));
                LINES.with_borrow_mut(|m| m.insert(id, lines.clone()));
                Some(lines)
            }
        },
        |snap: &Snapshot<Option<Option<Rc<Vec<String>>>>>| -> Element {
            match &snap.data {
                Some(Some(lines)) => badges(lines),
                _ => spacer().into(),
            }
        },
    )
    .into()
}

fn badges(lines: &[String]) -> Element {
    row(lines
        .iter()
        .map(|l| badge(l, 12.0).padding_xy(5.0, 1.0).into()))
    .wrap()
    .gap(4.0)
    .into()
}

fn badge(route: &str, size: f32) -> Container {
    let (bg, fg) = line_color(route);
    container(text(route).font(Font::BOLD).size(size).color(fg))
        .bg(bg)
        .radius(size * 0.4)
        .align(AlignItems::CENTER)
}

fn departures(uid: String) -> Element {
    stream_builder(
        uid.clone(),
        move |_| {
            poll(
                move || {
                    let ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis());
                    parse_passages(&get_text(&format!(
                        "{API}/PASSAGE?stopID={uid}&timestamp={ms}"
                    ))?)
                },
                |r| Some(if r.is_ok() { REFRESH } else { RETRY }),
            )
        },
        |snap: &Snapshot<Latest<Result<Vec<Passage>, String>>>| -> Element {
            match &snap.data.0 {
                None => message("Abfahrten werden geladen…"),
                Some(Err(e)) => message(&format!(
                    "Abfahrten konnten nicht geladen werden, neuer Versuch…\n{e}"
                )),
                Some(Ok(p)) if p.is_empty() => message("Derzeit keine Abfahrten."),
                Some(Ok(p)) => scroll(column(p.iter().map(departure)).gap(6.0))
                    .grow(1.0)
                    .into(),
            }
        },
    )
    .grow(1.0)
    .into()
}

fn departure(p: &Passage) -> Element {
    let live = p.stat == "PREDICTED";
    row([
        badge(&p.route, 16.0)
            .padding_xy(8.0, 4.0)
            .min_width(48.0)
            .into(),
        column([
            text(&p.direction).size(16.0).color(INK).into(),
            text(format!("Steig {}", p.plabel))
                .size(13.0)
                .color(MUTED)
                .into(),
        ])
        .grow(1.0)
        .shrink(1.0)
        .into(),
        text(&p.time)
            .font(Font::BOLD)
            .size(16.0)
            .color(if live { Color::hex(0x1e8a4c) } else { INK })
            .shrink(0.0)
            .into(),
    ])
    .align(AlignItems::CENTER)
    .gap(12.0)
    .padding_xy(12.0, 10.0)
    .bg(Color::WHITE)
    .radius(10.0)
    .into()
}

/// Official colors from ivb.at's line overview; regional and unlisted lines get grey.
const LINE_COLORS: &[(&str, u32)] = &[
    ("1", 0xedacaf),
    ("2", 0xca455f),
    ("2A", 0xca455f),
    ("3", 0xd26f7a),
    ("5", 0x8c4a5c),
    ("6", 0x0b6d38),
    ("STB", 0x640016),
    ("A", 0x0073a2),
    ("C", 0x89ceea),
    ("D", 0xf8a500),
    ("F", 0x8b13ab),
    ("J", 0xb457b4),
    ("K", 0xdbb3e5),
    ("M", 0xdb8a48),
    ("R", 0xee6700),
    ("T", 0x7fc700),
    ("W", 0x00a759),
    ("N1", 0x004572),
    ("N2", 0x004572),
    ("N3", 0x004572),
    ("N7", 0x004572),
    ("N8", 0x004572),
    ("HBB", 0xee88ba),
];

/// Badge background and the text color that reads on it.
fn line_color(route: &str) -> (Color, Color) {
    let rgb = LINE_COLORS
        .iter()
        .find(|(l, _)| *l == route)
        .map_or(0x5a6072, |&(_, c)| c);
    let [_, r, g, b] = rgb.to_be_bytes().map(u32::from);
    let light = r * 299 + g * 587 + b * 114 > 160_000;
    (Color::hex(rgb), if light { INK } else { Color::WHITE })
}

fn message(s: &str) -> Element {
    container(text(s).size(16.0).color(MUTED))
        .padding(24.0)
        .into()
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: dfui::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
    dfui::run_android(Bus, app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passages_skip_non_departures() {
        let json = r#"[
            {"smartinfo":{"route":"C","direction":"Sieglanger","time":"1 min","stat":"PREDICTED","plabel":"B","pnum":"2"}},
            {"linedirections":{"route":"3","direction":["X"]}},
            {"stopidname":"Hauptbahnhof"}
        ]"#;
        let p = parse_passages(json).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].route, "C");
    }

    #[test]
    fn line_colors_follow_ivb() {
        assert!(line_color("6").0 == Color::hex(0x0b6d38));
        assert!(line_color("6").1 == Color::WHITE);
        assert!(line_color("1").1 == INK);
        assert!(line_color("504").0 == Color::hex(0x5a6072));
    }

    #[test]
    fn lines_sort_ivb_first_without_duplicates() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            sort_lines(l(&["504", "R", "2", "STB", "420", "2"])),
            l(&["2", "STB", "R", "504", "420"])
        );
    }

    #[test]
    fn latest_keeps_newest() {
        let mut l = Latest::default();
        l.extend([1, 2]);
        l.extend([3]);
        assert_eq!(l.0, Some(3));
    }
}
