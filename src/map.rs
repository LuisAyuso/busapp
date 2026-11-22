//! The city map: OpenStreetMap data around Innsbruck, drawn with dfui-map
//! from the tiles in `assets/innsbruck.map` (see `tiles`).

#[cfg(test)]
mod tiles;

use std::cell::OnceCell;
use std::rc::Rc;

use dfui::path::Point;
use dfui::taffy::{AlignItems, JustifyContent};
use dfui::widgets::Overlays;
use dfui::widgets::{
    Styled, backdrop, button, column, container, icon_button, row, screen, scroll, spacer, text,
    text_input,
};
use dfui::{Color, Element, Font, Handle, Path, Rect, Shadow, Shape};
use dfui_map::{MapBuilder, MapData, map};

use crate::{
    ACCENT, INK, MAX_ROWS, MUTED, Pick, Stop, Ui, connections, gps, menu_button, network,
    open_stop, page_with,
};

/// Origin of the map's metres, degrees: the old town.
const CENTER: (f64, f64) = (47.2655, 11.3980);
/// Opening zoom, logical px per metre: the old town and the station.
const START_SCALE: f32 = 0.3;
/// Zoom a search result opens at, at least.
const FOUND_SCALE: f32 = 1.0;
const LAND: Color = Color::hex(0xf2efe9);
/// Walking pace over the straight-line distance, metres a minute: 80 m/min
/// on foot, less for streets that don't run straight to the stop.
const WALK_PACE: f32 = 60.0;
const CREDIT: &str = "© OpenStreetMap contributors";
const CREDIT_HEIGHT: f32 = 18.0;
/// Opacity of the white bands under the card and the buttons: faint, so the
/// map reads through them.
const BAND_ALPHA: f32 = 0.3;
/// A tap names the closest address within this many metres.
const ADDRESS_REACH: f32 = 60.0;
/// The closest stops to the pin, one color each, on the map and their buttons.
const NEAR: [Color; 3] = [
    Color::hex(0x1565c0),
    Color::hex(0x2e7d32),
    Color::hex(0x6a1b9a),
];

/// The dropped pin and the address or street it names, if any.
pub type Pin = (Point, Option<String>);
/// What the pin is called while it follows the phone.
const HERE: &str = "Mein Standort";
const ME: Color = Color::hex(0x1a73e8);

/// The pin: the phone's position while locating, else where it was dropped.
pub fn pin(ui: &Ui) -> Option<Pin> {
    match ui.locating {
        true => gps::fix().map(|f| (project(f), Some(HERE.into()))),
        false => ui.pin.clone(),
    }
}

/// A GPS fix in map metres.
fn project((lat, lon, _): gps::Fix) -> Point {
    MapBuilder::new(CENTER.0, CENTER.1).point(lat, lon)
}

/// The map, where each snapshot stop lies on it (None without coordinates),
/// and the places a search finds.
struct City {
    data: Rc<MapData>,
    stops: Vec<Option<Point>>,
    /// The lines serving each stop.
    lines: Vec<Vec<String>>,
    /// Street names and a point along each.
    streets: Vec<(String, Point)>,
    /// Street, house number, point; by street, then number.
    addresses: Vec<(String, String, Point)>,
}

/// The city, read on first use and kept. The map's tiles are decoded as
/// they first come into view.
fn city() -> Rc<City> {
    thread_local! {
        static CITY: OnceCell<Rc<City>> = const { OnceCell::new() };
    }
    CITY.with(|m| m.get_or_init(|| Rc::new(load())).clone())
}

fn load() -> City {
    let b = MapBuilder::new(CENTER.0, CENTER.1);
    let data = MapData::from_bytes(include_bytes!("../assets/innsbruck.map"));
    let streets = serde_json::from_str(include_str!("../assets/streets.json"));
    let rows: Vec<(String, String, f64, f64)> =
        serde_json::from_str(include_str!("../assets/addresses.json")).expect("addresses.json");
    let mut addresses: Vec<(String, String, Point)> = (rows.into_iter())
        .map(|(s, n, lat, lon)| (s, n, b.point(lat, lon)))
        .collect();
    let number = |n: &str| {
        n.split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse::<u32>()
            .ok()
    };
    addresses.sort_by(|a, b| (&a.0, number(&a.1), &a.1).cmp(&(&b.0, number(&b.1), &b.1)));
    City {
        data: Rc::new(data.expect("innsbruck.map")),
        stops: stops(&b),
        lines: lines(),
        streets: streets.expect("streets.json"),
        addresses,
    }
}

/// The lines serving each snapshot stop.
fn lines() -> Vec<Vec<String>> {
    let net = network::network();
    let mut lines = vec![Vec::new(); net.stops.len()];
    for (line, a, b, _) in &net.edges {
        for s in [*a, *b] {
            if !lines[s].contains(line) {
                lines[s].push(line.clone());
            }
        }
    }
    lines
}

/// Where each snapshot stop lies on the map; None without coordinates.
fn stops(b: &MapBuilder) -> Vec<Option<Point>> {
    (network::network().stops.iter())
        .map(|&(_, lat, lon)| (lat != 0.0).then(|| b.point(lat.into(), lon.into())))
        .collect()
}

/// The Karte page: the map with the city lines and stops, and a search
/// button by the title. A tap or a search drops a pin, names it after the
/// title and lists the closest stops, marked on the map in their colors.
pub fn page(h: Handle<Ui>, stops: &[Stop], ui: &Ui, ov: Handle<Overlays>) -> Element {
    let city = city();
    let pin = pin(ui);
    let near = pin.as_ref().map(|p| near(&city, p.0)).unwrap_or_default();
    let marks: Vec<(Point, Color)> = (near.iter().zip(NEAR))
        .filter_map(|(n, color)| Some((city.stops[n.stop]?, color)))
        .collect();
    let at = pin.as_ref().map(|p| p.0);
    // Accuracy in metres while the pin is the phone's position.
    let me = gps::fix().filter(|_| ui.locating).map(|f| f.2);
    let focus = match ui.locating {
        true => gps::first().map(project),
        false => ui.focus,
    };
    let tapped = city.clone();
    let m = map(&city.data)
        .start([0.0, 0.0], START_SCALE)
        .focus(focus.map(|p| (p, FOUND_SCALE)))
        .on_tap(move |cx, p| {
            let name = closest_address(&tapped.addresses, p);
            gps::stop();
            cx.update(h, move |u: &mut Ui| {
                (u.pin, u.focus, u.locating) = (Some((p, name)), None, false)
            })
        })
        .overlay(move |cx, to_px| {
            let r = cx.rect;
            let w = cx.measure(CREDIT, Font::DEFAULT, 11.0, None).width;
            // Above the navigation bar the map runs under.
            let [_, _, bottom, left] = dfui::insets();
            let y = r.y + r.h - bottom - CREDIT_HEIGHT;
            let back = Rect::new(r.x + left, y, w + 10.0, CREDIT_HEIGHT);
            cx.fill(back, 0.0, Color::WHITE.alpha(0.7));
            cx.text(
                CREDIT,
                Font::DEFAULT,
                11.0,
                INK,
                None,
                (back.x + 5.0, back.y + 2.0),
            );
            for &(p, color) in &marks {
                let [x, y] = to_px(p);
                cx.fill(Rect::new(x - 10.0, y - 10.0, 20.0, 20.0), 10.0, color);
                cx.fill(Rect::new(x - 4.0, y - 4.0, 8.0, 8.0), 4.0, Color::WHITE);
            }
            if let (Some(p), Some(acc)) = (at, me) {
                let [x, y] = to_px(p);
                let r = (to_px([p[0] + acc, p[1]])[0] - x).max(9.0);
                cx.fill(Rect::new(x - r, y - r, 2.0 * r, 2.0 * r), r, ME.alpha(0.15));
                cx.fill(Rect::new(x - 9.0, y - 9.0, 18.0, 18.0), 9.0, Color::WHITE);
                cx.fill(Rect::new(x - 6.5, y - 6.5, 13.0, 13.0), 6.5, ME);
            } else if let Some(p) = at {
                let [x, y] = to_px(p);
                cx.path(
                    &Path::polygon(&[[x - 7.0, y - 18.0], [x, y], [x + 7.0, y - 18.0]]),
                    ACCENT,
                );
                cx.fill(Rect::new(x - 11.0, y - 34.0, 22.0, 22.0), 11.0, ACCENT);
                cx.fill(Rect::new(x - 4.0, y - 27.0, 8.0, 8.0), 4.0, Color::WHITE);
            }
        });
    let mut panel: Vec<Element> = (near.iter().zip(NEAR))
        .filter_map(|(n, color)| {
            let mut walk = format!("{} m", (n.metres / 10.0).round() * 10.0);
            if n.rise.abs() >= SHOWN_CLIMB {
                let arrow = if n.rise > 0.0 { '↑' } else { '↓' };
                walk += &format!(" · {arrow}{} m", n.rise.abs().round());
            }
            open_stop(h, stops, n.stop, &walk, color)
        })
        .collect();
    if pin.is_some() {
        let trip = |label: &str, pick: Pick| -> Element {
            button(text(label).color(Color::WHITE))
                .color(ACCENT)
                .grow(1.0)
                .on_click(move |cx| {
                    gps::stop();
                    cx.update(h, move |u: &mut Ui| {
                        // A trip keeps the position the pin had.
                        (u.pin, u.locating) = (self::pin(u), false);
                        u.picking = Some(pick);
                        u.query.clear();
                    })
                })
                .into()
        };
        let trips = row([
            trip("Fahrt von hier", Pick::FromPin),
            trip("Fahrt hierher", Pick::ToPin),
        ]);
        panel.push(trips.gap(6.0).into());
    }
    let mut head: Vec<Element> = vec![menu_button(h, ov, Color::WHITE)];
    match &pin {
        Some((_, Some(name))) => head.push(text(name).size(17.0).color(INK).shrink(1.0).into()),
        None if ui.locating => {
            let wait = text("Standort wird gesucht…").size(16.0).color(MUTED);
            head.push(wait.shrink(1.0).into());
        }
        _ => {}
    }
    head.push(spacer().grow(1.0).into());
    if gps::AVAILABLE {
        let locate = icon_button(&crosshair())
            .color(if ui.locating { ME } else { INK })
            .on_click(move |cx| {
                gps::start();
                cx.update(h, |u: &mut Ui| (u.locating, u.focus) = (true, None))
            });
        head.push(locate.into());
    }
    let find = icon_button(&magnifier())
        .color(INK)
        .on_click(move |cx| cx.update(h, |u: &mut Ui| u.finding = true));
    head.push(find.into());
    let head = row(head)
        .align(AlignItems::CENTER)
        .gap(6.0)
        .padding_xy(4.0, 2.0);
    // The map fills the screen, under the system bars too; the card and the
    // buttons sit on faint bands that reach the screen's edges. Layers take
    // taps on their content, so those don't drop a pin.
    let [top, right, bottom, left] = dfui::insets();
    // Padded clear of the bars by [top, bottom] beyond 12 px.
    let band = |e: Element, [t, b]: [f32; 2]| {
        let mut c = container(e).bg(Color::WHITE.alpha(BAND_ALPHA));
        let px = dfui::taffy::LengthPercentage::length;
        c.style_mut().padding = dfui::taffy::Rect {
            top: px(12.0 + t),
            right: px(12.0 + right),
            bottom: px(12.0 + b),
            left: px(12.0 + left),
        };
        c
    };
    let head = band(card(head.into()), [top, 0.0]);
    let mut layers = vec![
        backdrop(container(m).bg(LAND).grow(1.0)).into(),
        screen(head).into(),
    ];
    if !panel.is_empty() {
        let panel = band(column(panel).gap(6.0).into(), [0.0, bottom + CREDIT_HEIGHT]);
        layers.push(screen(panel).justify(JustifyContent::END).into());
    }
    column(layers).fill().into()
}

/// A white rounded card with a soft shadow, to read over the map.
fn card(e: Element) -> Element {
    let shadow = Shadow {
        color: Color::BLACK.alpha(0.18),
        offset: (0.0, 2.0),
        blur: 8.0,
    };
    container(e)
        .bg(Color::WHITE)
        .radius(14.0)
        .shadow(shadow)
        .into()
}

/// What the pin is called in titles.
pub fn pin_name(ui: &Ui) -> String {
    let name = pin(ui).and_then(|p| p.1);
    name.unwrap_or_else(|| "Markierung".into())
}

/// Connections from `a` to the stops closest to the pin, by arrival there
/// after walking on from the stop.
pub fn trip_to_pin(stops: &[Stop], ui: &Ui, a: &Stop) -> Element {
    let trips = near_pin(stops, ui, false).into_iter();
    connections(trips.map(|(b, walk)| (a, b, None, Some(walk))).collect())
}

/// Connections from the stops closest to the pin to `b`, walking to the
/// stop first, by arrival.
pub fn trip_from_pin(stops: &[Stop], ui: &Ui, b: &Stop) -> Element {
    let trips = near_pin(stops, ui, true).into_iter();
    connections(trips.map(|(a, walk)| (a, b, Some(walk), None)).collect())
}

/// The stops closest to the pin and the walk between each and the pin,
/// to the stop when `to_stop`, else from it.
fn near_pin<'a>(stops: &'a [Stop], ui: &Ui, to_stop: bool) -> Vec<(&'a Stop, crate::plan::Walk)> {
    let Some((p, _)) = pin(ui) else {
        return Vec::new();
    };
    (near(&city(), p).into_iter())
        .filter_map(|n| {
            let name = &network::network().stops[n.stop].0;
            let stop = stops.iter().find(|s| &s.name == name)?;
            Some((stop, (name.clone(), n.minutes(to_stop))))
        })
        .collect()
}

/// The stops near `p` in the city, by effort including the climb.
fn near(city: &City, p: Point) -> Vec<Near> {
    let here = height(p);
    nearest(&city.stops, &city.lines, p, |i| {
        city.stops[i].map_or(0.0, |s| height(s) - here)
    })
}

/// Ground height at a map point, metres, from the bundled grid (see
/// `tiles::refresh_elevation`); 0 off the grid.
fn height(p: Point) -> f32 {
    /// South, west, rows' and columns' steps in degrees, columns, heights.
    type Grid = (f64, f64, f64, f64, usize, Vec<f32>);
    static GRID: std::sync::OnceLock<Grid> = std::sync::OnceLock::new();
    let (s, w, dlat, dlon, cols, h) = GRID.get_or_init(|| {
        serde_json::from_str(include_str!("../assets/elevation.json")).expect("elevation.json")
    });
    let (lat, lon) = degrees(p);
    let (y, x) = ((lat - s) / dlat, (lon - w) / dlon);
    let rows = h.len() / cols;
    if !(0.0..(rows - 1) as f64).contains(&y) || !(0.0..(cols - 1) as f64).contains(&x) {
        return 0.0;
    }
    let (r, c) = (y as usize, x as usize);
    let (fy, fx) = ((y - r as f64) as f32, (x - c as f64) as f32);
    let at = |r: usize, c: usize| h[r * cols + c];
    let south = at(r, c) * (1.0 - fx) + at(r, c + 1) * fx;
    let north = at(r + 1, c) * (1.0 - fx) + at(r + 1, c + 1) * fx;
    south * (1.0 - fy) + north * fy
}

/// A map point back in degrees, the inverse of `MapBuilder::point`.
fn degrees(p: Point) -> (f64, f64) {
    const R: f64 = 6_371_000.0;
    let lat = CENTER.0 - (f64::from(p[1]) / R).to_degrees();
    let lon = CENTER.1 + (f64::from(p[0]) / (R * CENTER.0.to_radians().cos())).to_degrees();
    (lat, lon)
}

/// The address search: a street, or a street and house number.
pub fn find(h: Handle<Ui>, query: &str) -> Element {
    let city = city();
    let hits = places(&city.streets, &city.addresses, query)
        .into_iter()
        .map(|(name, p)| {
            let label = text(&name).size(17.0).color(INK);
            button(label)
                .flat()
                .color(Color::WHITE)
                .justify(dfui::taffy::JustifyContent::START)
                .on_click(move |cx| {
                    let name = name.clone();
                    cx.update(h, move |u: &mut Ui| {
                        (u.pin, u.focus, u.finding) = (Some((p, Some(name))), Some(p), false)
                    })
                })
                .into()
        });
    let input = text_input(query)
        .placeholder("Straße und Hausnummer")
        .accent(ACCENT)
        .on_change(move |cx, v| cx.update(h, move |u: &mut Ui| u.address = v));
    page_with(
        h,
        text("Adresse suchen")
            .font(Font::BOLD)
            .size(24.0)
            .color(INK)
            .into(),
        |u| u.finding = false,
        [input.into(), scroll(column(hits).gap(2.0)).grow(1.0).into()],
    )
}

/// Streets whose name holds every word of `query`, or with a trailing house
/// number, their addresses whose number starts with it.
fn places(
    streets: &[(String, Point)],
    addresses: &[(String, String, Point)],
    query: &str,
) -> Vec<(String, Point)> {
    let q = query.to_lowercase();
    let mut words: Vec<&str> = q.split_whitespace().collect();
    let number = words.pop_if(|w| w.starts_with(|c: char| c.is_ascii_digit()));
    if words.is_empty() {
        return Vec::new();
    }
    let named = |s: &str| {
        let s = s.to_lowercase();
        words.iter().all(|w| s.contains(w))
    };
    match number {
        None => (streets.iter())
            .filter(|s| named(&s.0))
            .take(MAX_ROWS)
            .cloned()
            .collect(),
        Some(n) => (addresses.iter())
            .filter(|a| named(&a.0) && a.1.to_lowercase().starts_with(n))
            .take(MAX_ROWS)
            .map(|(s, n, p)| (format!("{s} {n}"), *p))
            .collect(),
    }
}

/// "Street number" of the address closest to `p`, within `ADDRESS_REACH`.
fn closest_address(addresses: &[(String, String, Point)], p: Point) -> Option<String> {
    let d = |a: &Point| (a[0] - p[0]).hypot(a[1] - p[1]);
    let (s, n, a) = addresses
        .iter()
        .min_by(|a, b| d(&a.2).total_cmp(&d(&b.2)))?;
    (d(a) <= ADDRESS_REACH).then(|| format!("{s} {n}"))
}

/// A magnifying glass, 24 units square.
fn magnifier() -> Shape {
    let ring = |r: f32| -> Vec<Point> {
        (0..24)
            .map(|i| {
                let a = i as f32 * std::f32::consts::TAU / 24.0;
                [10.0 + r * a.cos(), 10.0 + r * a.sin()]
            })
            .collect()
    };
    let mut path = Path::polygon(&ring(7.5));
    // The inner edge runs the other way, so the lens stays hollow.
    let inner: Vec<Point> = ring(5.5).into_iter().rev().collect();
    path.contours.extend(Path::polygon(&inner).contours);
    let handle = [[14.6, 13.2], [21.5, 20.1], [20.1, 21.5], [13.2, 14.6]];
    path.contours.extend(Path::polygon(&handle).contours);
    Shape::new(path, Rect::new(0.0, 0.0, 24.0, 24.0))
}

/// A crosshair, 24 units square: a ring with a dot and four ticks.
fn crosshair() -> Shape {
    let ring = |r: f32, back: bool| -> Vec<Point> {
        let pts = (0..24).map(|i| {
            let a = i as f32 * std::f32::consts::TAU / 24.0;
            [12.0 + r * a.cos(), 12.0 + r * a.sin()]
        });
        match back {
            true => pts.rev().collect(),
            false => pts.collect(),
        }
    };
    let mut path = Path::polygon(&ring(7.5, false));
    // The inner edge runs the other way, so the ring stays hollow.
    path.contours
        .extend(Path::polygon(&ring(5.5, true)).contours);
    path.contours
        .extend(Path::polygon(&ring(2.5, false)).contours);
    for [x, y, w, h] in [
        [11.0, 1.0, 2.0, 4.0],
        [11.0, 19.0, 2.0, 4.0],
        [1.0, 11.0, 4.0, 2.0],
        [19.0, 11.0, 4.0, 2.0],
    ] {
        path.contours
            .extend(Path::polygon(&[[x, y], [x + w, y], [x + w, y + h], [x, y + h]]).contours);
    }
    Shape::new(path, Rect::new(0.0, 0.0, 24.0, 24.0))
}

/// Stops counted to gauge how dense they are around a point.
const DENSITY_STOPS: usize = 3;
/// How far the stops are looked for, as a multiple of the distance to the
/// [`DENSITY_STOPS`]th closest, between these bounds in metres: a few blocks
/// downtown, further where stops are sparse.
const REACH: f32 = 2.0;
const MIN_REACH: f32 = 300.0;
const MAX_REACH: f32 = 1500.0;

/// Flat metres a metre of height difference counts as when ranking stops:
/// a 10 m climb takes about as long as 80 m on the flat (Naismith).
const CLIMB_WEIGHT: f32 = 8.0;
/// Minutes a 10 m climb adds to a walk.
const CLIMB_PACE: f32 = 10.0;
/// Least height difference worth showing on a stop's button, metres.
const SHOWN_CLIMB: f32 = 15.0;

/// A stop near a point: its snapshot index, how far it is on the map and
/// how much higher it lies, metres.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Near {
    stop: usize,
    metres: f32,
    rise: f32,
}

impl Near {
    /// What getting there takes, as metres on the flat.
    fn effort(&self) -> f32 {
        self.metres + CLIMB_WEIGHT * self.rise.abs()
    }

    /// Minutes on foot, `up` the stop or down from it.
    fn minutes(&self, up: bool) -> f64 {
        let climb = if up { self.rise } else { -self.rise };
        f64::from(self.metres / WALK_PACE + climb.max(0.0) / CLIMB_PACE)
    }
}

/// Up to `NEAR.len()` stops near `p`, by effort: the closest, then each
/// time the closest that adds a line the ones before don't serve, then the
/// closest left. Only stops within the reach count, unless there are none.
/// `rise` gives how much higher a stop lies than `p`.
fn nearest(
    stops: &[Option<Point>],
    lines: &[Vec<String>],
    p: Point,
    rise: impl Fn(usize) -> f32,
) -> Vec<Near> {
    let mut by = (stops.iter().enumerate())
        .filter_map(|(i, s)| {
            let s = (*s)?;
            let metres = (s[0] - p[0]).hypot(s[1] - p[1]);
            Some(Near {
                stop: i,
                metres,
                rise: rise(i),
            })
        })
        .collect::<Vec<_>>();
    by.sort_by(|a, b| a.effort().total_cmp(&b.effort()));
    let gauge = by
        .get(DENSITY_STOPS - 1)
        .or(by.last())
        .map_or(0.0, Near::effort);
    let reach = (REACH * gauge).clamp(MIN_REACH, MAX_REACH);
    let near = by.partition_point(|s| s.effort() <= reach).max(1);
    by.truncate(near);
    let mut picked: Vec<Near> = Vec::new();
    while picked.len() < NEAR.len() && !by.is_empty() {
        let new = |n: &Near| {
            let served = |l: &String| picked.iter().any(|m| lines[m.stop].contains(l));
            lines
                .get(n.stop)
                .is_some_and(|ls| ls.iter().any(|l| !served(l)))
        };
        let k = by.iter().position(new).unwrap_or(0);
        picked.push(by.remove(k));
    }
    picked
}

#[cfg(test)]
mod tests {
    #[test]
    fn innsbruck_builds() {
        let city = super::city();
        assert!(city.data.tiles() > 1000, "{} shapes", city.data.tiles());
        assert!(city.data.segments() > 100_000);
    }

    #[test]
    fn places_match_streets_then_numbers() {
        let streets = [
            ("Museumstraße".to_owned(), [0.0, 0.0]),
            ("Anichstraße".to_owned(), [1.0, 0.0]),
        ];
        let addresses = [
            ("Museumstraße".to_owned(), "1".to_owned(), [2.0, 0.0]),
            ("Museumstraße".to_owned(), "12".to_owned(), [3.0, 0.0]),
            ("Museumstraße".to_owned(), "3".to_owned(), [4.0, 0.0]),
        ];
        let names = |q| {
            super::places(&streets, &addresses, q)
                .into_iter()
                .map(|p| p.0)
                .collect::<Vec<_>>()
        };
        assert_eq!(names("str"), ["Museumstraße", "Anichstraße"]);
        assert_eq!(names("museum 1"), ["Museumstraße 1", "Museumstraße 12"]);
        assert_eq!(names("12"), Vec::<String>::new());
        let near = super::closest_address(&addresses, [3.5, 10.0]);
        assert_eq!(near.as_deref(), Some("Museumstraße 12"));
        assert_eq!(super::closest_address(&addresses, [0.0, 500.0]), None);
    }

    #[test]
    fn nearest_stops_come_closest_first() {
        let stops = [
            Some([0.0, 100.0]),
            None,
            Some([0.0, 10.0]),
            Some([50.0, 0.0]),
        ];
        let lines = vec![
            vec!["1".to_owned()],
            vec![],
            vec!["2".into()],
            vec!["3".into()],
        ];
        let n = super::nearest(&stops, &lines, [0.0, 0.0], |_| 0.0);
        let got: Vec<(usize, f32)> = n.iter().map(|n| (n.stop, n.metres)).collect();
        assert_eq!(got, [(2, 10.0), (3, 50.0), (0, 100.0)]);
    }

    #[test]
    fn nearest_stops_vary_their_lines_within_reach() {
        let line = |l: &str| vec![l.to_owned()];
        let stops = [
            Some([0.0, 100.0]),
            Some([0.0, 120.0]),
            Some([0.0, 150.0]),
            Some([0.0, 250.0]),
            Some([0.0, 5000.0]),
        ];
        let lines = [line("1"), line("1"), line("1"), line("2"), line("3")];
        let n = super::nearest(&stops, &lines, [0.0, 0.0], |_| 0.0);
        // Line 2 comes before the second stop of line 1; line 3 is beyond
        // the reach of a place this dense in stops.
        let picked: Vec<usize> = n.iter().map(|s| s.stop).collect();
        assert_eq!(picked, [0, 3, 1]);
        // Far from everything, the closest still shows.
        let n = super::nearest(&stops, &lines, [0.0, 30_000.0], |_| 0.0);
        assert_eq!(n.iter().map(|s| s.stop).collect::<Vec<_>>(), [4]);
    }

    #[test]
    fn stops_far_below_rank_behind_level_ones() {
        let line = |l: &str| vec![l.to_owned()];
        // The closest lies 100 m below, like the valley under Hungerburg.
        let stops = [Some([0.0, 200.0]), Some([0.0, 600.0])];
        let lines = [line("1"), line("2")];
        let rise = |i: usize| if i == 0 { -100.0 } else { 0.0 };
        let n = super::nearest(&stops, &lines, [0.0, 0.0], rise);
        assert_eq!(n[0].stop, 1);
        // Walking back up from it takes 10 minutes more than down to it.
        let below = n.iter().find(|n| n.stop == 0).unwrap();
        assert!((below.minutes(false) - below.minutes(true) - 10.0).abs() < 1e-6);
    }

    #[test]
    fn heights_rise_from_the_old_town_to_hungerburg() {
        let b = dfui_map::MapBuilder::new(super::CENTER.0, super::CENTER.1);
        let (town, hungerburg) = (b.point(47.2687, 11.3935), b.point(47.2865, 11.3990));
        let (low, high) = (super::height(town), super::height(hungerburg));
        assert!((560.0..610.0).contains(&low), "{low}");
        assert!((840.0..900.0).contains(&high), "{high}");
        let (lat, lon) = super::degrees(town);
        assert!((lat - 47.2687).abs() < 1e-6 && (lon - 11.3935).abs() < 1e-6);
    }
}
