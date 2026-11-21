//! The city map: OpenStreetMap data around central Innsbruck, bundled in
//! `assets/innsbruck.json` and drawn with dfui-map.

use std::cell::OnceCell;
use std::rc::Rc;

use dfui::path::Point;
use dfui::widgets::{Styled, column, container, text};
use dfui::{Color, Element, Font, Text};
use dfui_map::{Layer, MapBuilder, MapData, map};
use serde_json::Value;

use crate::MUTED;

/// Where `assets/innsbruck.json` is centred, degrees; it spans 4 km.
const CENTER: (f64, f64) = (47.2655, 11.3980);
/// Opening zoom, logical px per metre: the old town and the station.
const START_SCALE: f32 = 0.3;
const LAND: Color = Color::hex(0xf2efe9);

/// The map, built on first use (a few hundred ms) and kept.
fn data() -> Rc<MapData> {
    thread_local! {
        static MAP: OnceCell<Rc<MapData>> = const { OnceCell::new() };
    }
    MAP.with(|m| {
        m.get_or_init(|| Rc::new(build(include_str!("../assets/innsbruck.json"))))
            .clone()
    })
}

/// The pannable, zoomable map with the credit OSM's licence (ODbL) asks for.
pub fn view() -> Element {
    let credit = text("© OpenStreetMap contributors").size(12.0).color(MUTED);
    let m = map(&data()).start([0.0, 0.0], START_SCALE);
    let m = container(m).bg(LAND).grow(1.0);
    column([m.into(), credit.into()]).gap(4.0).grow(1.0).into()
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

fn build(json: &str) -> MapData {
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
    let mut names = Vec::new();
    for e in osm["elements"].as_array().into_iter().flatten() {
        add(&mut b, &style, &mut names, e);
    }
    // Earlier labels win the spacing: names of wide roads first.
    names.sort_by(|a, b| b.2.total_cmp(&a.2));
    let text = Text::default();
    for (pts, name, w) in &names {
        b.label(style.names, pts, &text, Font::DEFAULT, name, w * 0.75);
    }
    b.build()
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
        let data = super::data();
        assert!(data.tiles() > 1000, "{} shapes", data.tiles());
    }

    /// Downloads the area from Overpass and rewrites `assets/innsbruck.json`
    /// with only the tags `add` reads and coordinates to ~0.1 m.
    #[test]
    #[ignore = "downloads ~20 MB from a public Overpass instance"]
    fn refresh_map() {
        let (lat, lon) = super::CENTER;
        let (dlat, dlon) = (2.0 / 111.0, 2.0 / 111.0 / lat.to_radians().cos());
        let bbox = format!(
            "{},{},{},{}",
            lat - dlat,
            lon - dlon,
            lat + dlat,
            lon + dlon
        );
        let query = format!(
            "[out:json][timeout:120];(
              way[building]({bbox}); way[highway]({bbox}); way[railway=rail]({bbox});
              way[waterway]({bbox}); way[natural~\"^(water|wood)$\"]({bbox});
              way[leisure=park]({bbox}); way[landuse~\"^(grass|forest|meadow)$\"]({bbox});
              relation[natural=water]({bbox}); relation[building]({bbox});
            );out geom;"
        );
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
                    .header("User-Agent", "busapp-map-refresh")
                    .send_form([("data", query.as_str())])
                    .and_then(|mut r| r.body_mut().with_config().limit(200 << 20).read_to_string())
                    .ok()
            })
            .expect("every Overpass instance is busy: retry in a minute");
        let osm: Value = serde_json::from_str(&body).unwrap();
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
