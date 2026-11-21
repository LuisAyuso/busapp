//! Turns the downloaded OpenStreetMap data (`assets/innsbruck.json`) and
//! the line network into the tiles the app bundles, and refreshes the
//! downloads. Only tests run this: the app reads the finished tiles.

use std::collections::HashMap;

use dfui::path::Point;
use dfui::{Color, Font, Text};
use dfui_map::{Layer, MapBuilder, MapData};
use serde_json::{Map, Value, json};

use super::{CENTER, stops};
use crate::{INK, SHOWN_LINES, line_color, network};

const MAP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/innsbruck.map");
const STREETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/streets.json");

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

/// The map, and each street name with a point along it.
fn draw(json: &str) -> (MapData, Vec<(String, Point)>) {
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
    let stops = stops(&b);
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
    (b.build(), streets)
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

#[test]
fn bundled_tiles_match_the_data() {
    let (data, streets) = draw(include_str!("../../assets/innsbruck.json"));
    let stale = "stale: run `cargo test --lib write_tiles -- --ignored`";
    assert!(
        data.to_bytes() == include_bytes!("../../assets/innsbruck.map"),
        "{stale}"
    );
    let bundled: Vec<(String, Point)> =
        serde_json::from_str(include_str!("../../assets/streets.json")).unwrap();
    assert!(streets == bundled, "{stale}");
}

/// Rewrites `assets/innsbruck.map` and `assets/streets.json` from the
/// downloaded data and the line network.
#[test]
#[ignore = "rewrites the bundled tiles"]
fn write_tiles() {
    let json = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/innsbruck.json"
    ));
    let (data, streets) = draw(&json.unwrap());
    std::fs::write(MAP, data.to_bytes()).unwrap();
    std::fs::write(STREETS, serde_json::to_string(&streets).unwrap()).unwrap();
}

/// South, west, north, east around the stops of the lines the map draws,
/// with ~500 m to spare, cut into `n` × `n` boxes: public Overpass
/// instances time out on the whole area at once.
fn bboxes(n: usize) -> Vec<String> {
    let net = crate::network::network();
    let shown = (net.trips.iter()).filter(|t| crate::SHOWN_LINES.contains(&t.0.as_str()));
    let (mut s, mut w, mut no, mut e) = (90.0_f64, 180.0_f64, -90.0_f64, -180.0_f64);
    for &i in shown.flat_map(|t| &t.2) {
        let (_, lat, lon) = net.stops[i];
        let (lat, lon) = (f64::from(lat), f64::from(lon));
        if lat != 0.0 {
            (s, w, no, e) = (s.min(lat), w.min(lon), no.max(lat), e.max(lon));
        }
    }
    let m = 0.005;
    let (s, w, no, e) = (s - m, w - m * 1.5, no + m, e + m * 1.5);
    let (dlat, dlon) = ((no - s) / n as f64, (e - w) / n as f64);
    let corner = |i: usize, j: usize| (s + dlat * i as f64, w + dlon * j as f64);
    (0..n * n)
        .map(|k| {
            let ((s, w), (no, e)) = (corner(k / n, k % n), corner(k / n + 1, k % n + 1));
            format!("{s:.6},{w:.6},{no:.6},{e:.6}")
        })
        .collect()
}

/// The elements `query` finds in each of `n` × `n` boxes, each once.
fn overpass_tiled(query: impl Fn(&str) -> String, n: usize) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (i, b) in bboxes(n).iter().enumerate() {
        let osm = overpass(&query(b));
        let elements = osm["elements"].as_array().cloned().unwrap_or_default();
        eprintln!("box {}/{}: {} elements", i + 1, n * n, elements.len());
        for e in elements {
            if seen.insert((e["type"].to_string(), e["id"].to_string())) {
                out.push(e);
            }
        }
    }
    out
}

fn overpass(query: &str) -> Value {
    // Public instances turn queries away when busy; try each, then wait.
    let servers = [
        "https://z.overpass-api.de/api/interpreter",
        "https://overpass-api.de/api/interpreter",
        "https://overpass.private.coffee/api/interpreter",
        "https://maps.mail.ru/osm/tools/overpass/api/interpreter",
    ];
    for round in 0..5 {
        if round > 0 {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
        let body = servers.iter().find_map(|url| {
            let r = ureq::post(*url)
                .config()
                .timeout_global(Some(std::time::Duration::from_secs(300)))
                .build()
                .header("User-Agent", "busapp-map-refresh")
                .send_form([("data", query)])
                .and_then(|mut r| r.body_mut().with_config().limit(1 << 30).read_to_string());
            if let Err(e) = &r {
                eprintln!("{url}: {e}");
            }
            r.ok()
        });
        if let Some(osm) = body.and_then(|b| serde_json::from_str(&b).ok()) {
            return osm;
        }
    }
    panic!("every Overpass instance stayed busy");
}

/// Rewrites `assets/addresses.json` as `[street, number, lat, lon]` rows.
#[test]
#[ignore = "downloads a few MB from a public Overpass instance"]
fn refresh_addresses() {
    let elements = overpass_tiled(
        |bbox| {
            format!(
                "[out:json][timeout:180];nwr[\"addr:street\"][\"addr:housenumber\"]({bbox});out center;"
            )
        },
        2,
    );
    let mut rows: Vec<(String, String, f64, f64)> = (elements.iter())
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
    let raw = overpass_tiled(
        |bbox| {
            format!(
                "[out:json][timeout:300];(
                  way[building]({bbox}); way[highway]({bbox}); way[railway=rail]({bbox});
                  way[waterway]({bbox}); way[natural~\"^(water|wood)$\"]({bbox});
                  way[leisure=park]({bbox}); way[landuse~\"^(grass|forest|meadow)$\"]({bbox});
                  relation[natural=water]({bbox}); relation[building]({bbox});
                );out geom;"
            )
        },
        3,
    );
    const KEEP: [&str; 8] = [
        "name", "building", "highway", "railway", "waterway", "natural", "leisure", "landuse",
    ];
    let geometry = |g: &Value| -> Value {
        let round = |v: &Value| (v.as_f64().unwrap_or(0.0) * 1e6).round() / 1e6;
        let pts = g.as_array().into_iter().flatten();
        pts.map(|p| json!({"lat": round(&p["lat"]), "lon": round(&p["lon"])}))
            .collect()
    };
    let elements: Vec<Value> = (raw.iter())
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
