//! The city map: OpenStreetMap data around central Innsbruck, bundled in
//! `assets/innsbruck.json` and drawn with dfui-map.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::rc::Rc;

use dfui::path::Point;
use dfui::taffy::AlignItems;
use dfui::widgets::{
    Styled, button, column, container, icon_button, row, scroll, spacer, text, text_input,
};
use dfui::{Color, Element, Font, Handle, Path, Rect, Shape, Text};
use dfui_map::{Layer, MapBuilder, MapData, map};
use serde_json::Value;

use crate::{
    ACCENT, INK, MAX_ROWS, MUTED, SHOWN_LINES, Stop, Ui, line_color, network, open_stop, page_with,
};

/// Where `assets/innsbruck.json` is centred, degrees; it spans 4 km.
const CENTER: (f64, f64) = (47.2655, 11.3980);
/// Opening zoom, logical px per metre: the old town and the station.
const START_SCALE: f32 = 0.3;
/// Zoom a search result opens at, at least.
const FOUND_SCALE: f32 = 1.0;
const LAND: Color = Color::hex(0xf2efe9);
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

/// The map, where each snapshot stop lies on it (None without coordinates),
/// and the places a search finds.
struct City {
    data: Rc<MapData>,
    stops: Vec<Option<Point>>,
    /// Street names and a point along each.
    streets: Vec<(String, Point)>,
    /// Street, house number, point; by street, then number.
    addresses: Vec<(String, String, Point)>,
}

/// The city, built on first use (a few hundred ms) and kept.
fn city() -> Rc<City> {
    thread_local! {
        static CITY: OnceCell<Rc<City>> = const { OnceCell::new() };
    }
    CITY.with(|m| {
        m.get_or_init(|| {
            Rc::new(build(
                include_str!("../assets/innsbruck.json"),
                include_str!("../assets/addresses.json"),
            ))
        })
        .clone()
    })
}

/// The Karte page: the map with the city lines and stops, and a search
/// button by the title. A tap or a search drops a pin, names it after the
/// title and lists the closest stops, marked on the map in their colors.
pub fn page(h: Handle<Ui>, stops: &[Stop], ui: &Ui) -> Element {
    let city = city();
    let pin = ui.pin.clone();
    let near = pin
        .as_ref()
        .map(|p| nearest(&city.stops, p.0))
        .unwrap_or_default();
    let marks: Vec<(Point, Color)> = (near.iter().zip(NEAR))
        .filter_map(|(&(c, _), color)| Some((city.stops[c]?, color)))
        .collect();
    let at = pin.as_ref().map(|p| p.0);
    let tapped = city.clone();
    let m = map(&city.data)
        .start([0.0, 0.0], START_SCALE)
        .focus(ui.focus.map(|p| (p, FOUND_SCALE)))
        .on_tap(move |cx, p| {
            let name = closest_address(&tapped.addresses, p);
            cx.update(h, move |u: &mut Ui| {
                (u.pin, u.focus) = (Some((p, name)), None)
            })
        })
        .overlay(move |cx, to_px| {
            for &(p, color) in &marks {
                let [x, y] = to_px(p);
                cx.fill(Rect::new(x - 10.0, y - 10.0, 20.0, 20.0), 10.0, color);
                cx.fill(Rect::new(x - 4.0, y - 4.0, 8.0, 8.0), 4.0, Color::WHITE);
            }
            if let Some(p) = at {
                let [x, y] = to_px(p);
                cx.path(
                    &Path::polygon(&[[x - 7.0, y - 18.0], [x, y], [x + 7.0, y - 18.0]]),
                    ACCENT,
                );
                cx.fill(Rect::new(x - 11.0, y - 34.0, 22.0, 22.0), 11.0, ACCENT);
                cx.fill(Rect::new(x - 4.0, y - 27.0, 8.0, 8.0), 4.0, Color::WHITE);
            }
        });
    let mut body = vec![container(m).bg(LAND).grow(1.0).into()];
    body.extend(near.iter().zip(NEAR).filter_map(|(&(c, d), color)| {
        let walk = format!("{} m", (d / 10.0).round() * 10.0);
        open_stop(h, stops, c, &walk, color)
    }));
    body.push(
        text("© OpenStreetMap contributors")
            .size(12.0)
            .color(MUTED)
            .into(),
    );
    let mut head = vec![text("Karte").font(Font::BOLD).size(24.0).color(INK).into()];
    if let Some((_, Some(name))) = &pin {
        head.push(text(name).size(17.0).color(MUTED).shrink(1.0).into());
    }
    let find = icon_button(&magnifier())
        .color(INK)
        .on_click(move |cx| cx.update(h, |u: &mut Ui| u.finding = true));
    head.extend([spacer().grow(1.0).into(), find.into()]);
    let head = row(head).align(AlignItems::CENTER).gap(10.0).grow(1.0);
    page_with(
        h,
        head.into(),
        |u| u.screen = Default::default(),
        [column(body).gap(4.0).grow(1.0).into()],
    )
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

/// The closest stops to `p` as (snapshot index, metres), closest first.
fn nearest(stops: &[Option<Point>], p: Point) -> Vec<(usize, f32)> {
    let mut by = (stops.iter().enumerate())
        .filter_map(|(i, s)| s.map(|s| (i, (s[0] - p[0]).hypot(s[1] - p[1]))))
        .collect::<Vec<_>>();
    by.sort_by(|a, b| a.1.total_cmp(&b.1));
    by.truncate(NEAR.len());
    by
}

/// A closed circle of `r` metres around `c`.
fn circle(c: Point, r: f32) -> Vec<Point> {
    let mut pts: Vec<Point> = (0..16)
        .map(|i| {
            let a = i as f32 * std::f32::consts::TAU / 16.0;
            [c[0] + r * a.cos(), c[1] + r * a.sin()]
        })
        .collect();
    pts.push(pts[0]);
    pts
}

struct Style {
    green: Layer,
    water: Layer,
    buildings: Layer,
    rail: Layer,
    paths: Layer,
    minor: Layer,
    major: Layer,
    names: Layer,
}

/// A street name to place: its line, name and road width.
type Name = (Vec<Point>, String, f32);

fn build(json: &str, addresses: &str) -> City {
    let osm: Value = serde_json::from_str(json).expect("innsbruck.json");
    let mut b = MapBuilder::new(CENTER.0, CENTER.1);
    let style = Style {
        green: b.layer(Color::hex(0xc8e6b0), 0.0),
        water: b.layer(Color::hex(0xaad3df), 0.0),
        buildings: b.layer(Color::hex(0xd9d0c9), 0.4),
        rail: b.layer(Color::hex(0x9a9a9a), 0.0),
        // Footways, cycleways, steps: under a px wide below this.
        paths: b.layer(Color::WHITE, 0.4),
        minor: b.layer(Color::WHITE, 0.0),
        major: b.layer(Color::hex(0xf8d49c), 0.0),
        names: b.layer(Color::hex(0x5c5852), 0.0),
    };
    let net = network::network();
    let stops: Vec<Option<Point>> = (net.stops.iter())
        .map(|&(_, lat, lon)| (lat != 0.0).then(|| b.point(lat.into(), lon.into())))
        .collect();
    // ponytail: straight from stop to stop, and shared stretches show only the last line drawn; route geometry from OSM if that misleads.
    for &line in SHOWN_LINES.iter().rev() {
        let layer = b.layer(line_color(line).0, 0.0);
        for (_, _, run) in net.trips.iter().filter(|t| t.0 == line) {
            let pts: Vec<Point> = run.iter().filter_map(|&i| stops[i]).collect();
            b.line(layer, &pts, 8.0);
        }
    }
    let (ring, dot) = (b.layer(INK, 0.0), b.layer(Color::WHITE, 0.0));
    for &p in stops.iter().flatten() {
        b.area(ring, &[circle(p, 11.0)], &[]);
        b.area(dot, &[circle(p, 7.0)], &[]);
    }
    let mut names = Vec::new();
    for e in osm["elements"].as_array().into_iter().flatten() {
        add(&mut b, &style, &mut names, e);
    }
    // Earlier labels win the spacing: names of wide roads first.
    names.sort_by(|a, b| b.2.total_cmp(&a.2));
    let text = Text::default();
    let mut streets = HashMap::new();
    for (pts, name, w) in &names {
        b.label(style.names, pts, &text, Font::DEFAULT, name, w * 0.75);
        streets.entry(name.clone()).or_insert(pts[pts.len() / 2]);
    }
    let mut streets: Vec<(String, Point)> = streets.into_iter().collect();
    streets.sort_by(|a, b| a.0.cmp(&b.0));
    let rows: Vec<(String, String, f64, f64)> =
        serde_json::from_str(addresses).expect("addresses.json");
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
        data: Rc::new(b.build()),
        stops,
        streets,
        addresses,
    }
}

fn ring(b: &MapBuilder, geometry: &Value) -> Vec<Point> {
    let pts = geometry.as_array().map(Vec::as_slice).unwrap_or_default();
    pts.iter()
        .filter_map(|p| Some(b.point(p["lat"].as_f64()?, p["lon"].as_f64()?)))
        .collect()
}

fn add(b: &mut MapBuilder, s: &Style, names: &mut Vec<Name>, e: &Value) {
    let tag = |k: &str| e["tags"][k].as_str().unwrap_or("");
    let area_layer = match () {
        _ if !tag("building").is_empty() => Some(s.buildings),
        _ if tag("natural") == "water" || tag("waterway") == "riverbank" => Some(s.water),
        _ if tag("natural") == "wood" || !tag("leisure").is_empty() => Some(s.green),
        _ if !tag("landuse").is_empty() => Some(s.green),
        _ => None,
    };
    if e["type"] == "relation" {
        let Some(layer) = area_layer else { return };
        let (mut outers, mut holes) = (Vec::new(), Vec::new());
        // ponytail: only member ways that close on themselves; stitch open ones if big areas go missing.
        for m in e["members"].as_array().into_iter().flatten() {
            let r = ring(b, &m["geometry"]);
            if r.len() > 3 && r.first() == r.last() {
                match m["role"].as_str() {
                    Some("inner") => holes.push(r),
                    _ => outers.push(r),
                }
            }
        }
        b.area(layer, &outers, &holes);
        return;
    }
    let pts = ring(b, &e["geometry"]);
    let road = match tag("highway") {
        "motorway" | "trunk" | "primary" | "secondary" => Some((s.major, 16.0)),
        "tertiary" => Some((s.major, 12.0)),
        "residential" | "unclassified" | "living_street" => Some((s.minor, 9.0)),
        "service" | "pedestrian" => Some((s.minor, 5.0)),
        "" => None,
        _ => Some((s.paths, 2.5)),
    };
    let line = road.or(match tag("waterway") {
        "river" => Some((s.water, 20.0)),
        "canal" => Some((s.water, 10.0)),
        "stream" | "ditch" | "drain" => Some((s.water, 3.0)),
        _ if tag("railway") == "rail" => Some((s.rail, 2.0)),
        _ => None,
    });
    match (line, area_layer) {
        (Some((layer, w)), _) => {
            b.line(layer, &pts, w);
            if let Some(name) = e["tags"]["name"].as_str().filter(|_| road.is_some()) {
                names.push((pts, name.to_owned(), w));
            }
        }
        (None, Some(layer)) if pts.first() == pts.last() => b.area(layer, &[pts], &[]),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    #[test]
    fn innsbruck_builds() {
        let city = super::city();
        assert!(city.data.tiles() > 1000, "{} shapes", city.data.tiles());
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
        let n = super::nearest(&stops, [0.0, 0.0]);
        assert_eq!(n, [(2, 10.0), (3, 50.0), (0, 100.0)]);
    }

    /// South, west, north, east of the 4 km square around `CENTER`.
    fn bbox() -> String {
        let (lat, lon) = super::CENTER;
        let (dlat, dlon) = (2.0 / 111.0, 2.0 / 111.0 / lat.to_radians().cos());
        format!(
            "{},{},{},{}",
            lat - dlat,
            lon - dlon,
            lat + dlat,
            lon + dlon
        )
    }

    fn overpass(query: &str) -> Value {
        // Public instances turn queries away when busy; try each.
        let servers = [
            "https://overpass-api.de/api/interpreter",
            "https://overpass.private.coffee/api/interpreter",
            "https://maps.mail.ru/osm/tools/overpass/api/interpreter",
        ];
        let body = servers
            .iter()
            .find_map(|url| {
                ureq::post(*url)
                    .config()
                    .timeout_global(Some(std::time::Duration::from_secs(180)))
                    .build()
                    .header("User-Agent", "busapp-map-refresh")
                    .send_form([("data", query)])
                    .and_then(|mut r| r.body_mut().with_config().limit(200 << 20).read_to_string())
                    .ok()
            })
            .expect("every Overpass instance is busy: retry in a minute");
        serde_json::from_str(&body).unwrap()
    }

    /// Rewrites `assets/addresses.json` as `[street, number, lat, lon]` rows.
    #[test]
    #[ignore = "downloads a few MB from a public Overpass instance"]
    fn refresh_addresses() {
        let bbox = bbox();
        let osm = overpass(&format!(
            "[out:json][timeout:120];nwr[\"addr:street\"][\"addr:housenumber\"]({bbox});out center;"
        ));
        let mut rows: Vec<(String, String, f64, f64)> = (osm["elements"].as_array().into_iter())
            .flatten()
            .filter_map(|e| {
                let at = if e["center"].is_object() {
                    &e["center"]
                } else {
                    e
                };
                let round = |v: &Value| Some((v.as_f64()? * 1e6).round() / 1e6);
                Some((
                    e["tags"]["addr:street"].as_str()?.to_owned(),
                    e["tags"]["addr:housenumber"].as_str()?.to_owned(),
                    round(&at["lat"])?,
                    round(&at["lon"])?,
                ))
            })
            .collect();
        rows.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        rows.dedup_by(|a, b| (&a.0, &a.1) == (&b.0, &b.1));
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/addresses.json");
        std::fs::write(path, serde_json::to_string(&rows).unwrap()).unwrap();
    }

    /// Downloads the area from Overpass and rewrites `assets/innsbruck.json`
    /// with only the tags `add` reads and coordinates to ~0.1 m.
    #[test]
    #[ignore = "downloads ~20 MB from a public Overpass instance"]
    fn refresh_map() {
        let bbox = bbox();
        let query = format!(
            "[out:json][timeout:120];(
              way[building]({bbox}); way[highway]({bbox}); way[railway=rail]({bbox});
              way[waterway]({bbox}); way[natural~\"^(water|wood)$\"]({bbox});
              way[leisure=park]({bbox}); way[landuse~\"^(grass|forest|meadow)$\"]({bbox});
              relation[natural=water]({bbox}); relation[building]({bbox});
            );out geom;"
        );
        let osm = overpass(&query);
        const KEEP: [&str; 8] = [
            "name", "building", "highway", "railway", "waterway", "natural", "leisure", "landuse",
        ];
        let geometry = |g: &Value| -> Value {
            let round = |v: &Value| (v.as_f64().unwrap_or(0.0) * 1e6).round() / 1e6;
            let pts = g.as_array().into_iter().flatten();
            pts.map(|p| json!({"lat": round(&p["lat"]), "lon": round(&p["lon"])}))
                .collect()
        };
        let elements: Vec<Value> = (osm["elements"].as_array().into_iter().flatten())
            .map(|e| {
                let tags: Map<String, Value> = (e["tags"].as_object().into_iter().flatten())
                    .filter(|(k, _)| KEEP.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let mut out = json!({"type": e["type"], "tags": tags});
                if e["type"] == "relation" {
                    let ways = e["members"].as_array().into_iter().flatten();
                    out["members"] = ways
                        .filter(|m| m["type"] == "way")
                        .map(|m| json!({"role": m["role"], "geometry": geometry(&m["geometry"])}))
                        .collect();
                } else {
                    out["geometry"] = geometry(&e["geometry"]);
                }
                out
            })
            .collect();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/innsbruck.json");
        std::fs::write(path, json!({"elements": elements}).to_string()).unwrap();
    }
}
