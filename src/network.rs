//! The line network as a graph: stops joined by the lines between them,
//! weighted by travel minutes and laid out by forces around a chosen stop.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::OnceLock;

use dfui::path::Stroke;
use dfui::taffy::{Dimension, Size, Style};
use dfui::{Color, Element, Event, EventCx, Font, Instant, OnChange, PaintCx, Path, Rect, Widget};
use serde::{Deserialize, Serialize};

use crate::{ACCENT, INK, MUTED, line_color};

/// Snapshot written by the `refresh_network` test; IVB's API has no
/// per-line endpoint, so the topology takes a crawl of every stop.
#[derive(Serialize, Deserialize)]
pub struct Network {
    /// Name, latitude, longitude (0 when unknown).
    pub stops: Vec<(String, f32, f32)>,
    /// Line, stop index, stop index (smaller first), minutes between them.
    pub edges: Vec<(String, usize, usize, f32)>,
    /// Line, direction, its longest run of stop indices in that direction.
    pub trips: Vec<(String, String, Vec<usize>)>,
}

pub fn network() -> &'static Network {
    static NET: OnceLock<Network> = OnceLock::new();
    NET.get_or_init(|| {
        serde_json::from_str(include_str!("../assets/network.json")).expect("network.json")
    })
}

impl Network {
    pub fn lines(&self) -> Vec<String> {
        crate::sort_lines(self.edges.iter().map(|e| e.0.clone()).collect())
    }
}

/// Two stops joined by one or more lines, shown side by side.
pub struct Pair {
    pub a: usize,
    pub b: usize,
    pub minutes: f32,
    /// Indices into `lines`, in their order.
    pub lines: Vec<usize>,
}

/// Stop pairs served by the lines not in `hidden`.
pub fn pairs(net: &Network, lines: &[String], hidden: &HashSet<String>) -> Vec<Pair> {
    let mut at: HashMap<(usize, usize), usize> = HashMap::new();
    let mut out: Vec<Pair> = vec![];
    for (line, a, b, m) in &net.edges {
        if hidden.contains(line) {
            continue;
        }
        let Some(l) = lines.iter().position(|x| x == line) else {
            continue;
        };
        let i = *at.entry((*a, *b)).or_insert_with(|| {
            out.push(Pair {
                a: *a,
                b: *b,
                minutes: *m,
                lines: vec![],
            });
            out.len() - 1
        });
        let p = &mut out[i];
        p.minutes = p.minutes.min(*m);
        if !p.lines.contains(&l) {
            p.lines.push(l);
        }
    }
    for p in &mut out {
        p.lines.sort();
    }
    out
}

/// Travel minutes from `from` to every stop over `pairs`, ignoring
/// waits and changes; infinite where unreachable.
pub fn distances(n: usize, pairs: &[Pair], from: usize) -> Vec<f32> {
    let mut adj = vec![vec![]; n];
    for p in pairs {
        adj[p.a].push((p.b, p.minutes));
        adj[p.b].push((p.a, p.minutes));
    }
    let mut dist = vec![f32::INFINITY; n];
    dist[from] = 0.0;
    // Tenths of a minute as integers, since f32 isn't Ord.
    let mut heap = BinaryHeap::from([Reverse((0u32, from))]);
    while let Some(Reverse((d, u))) = heap.pop() {
        if d as f32 / 10.0 > dist[u] {
            continue;
        }
        for &(v, m) in &adj[u] {
            let nd = dist[u] + m;
            if nd < dist[v] {
                dist[v] = nd;
                heap.push(Reverse(((nd * 10.0).round() as u32, v)));
            }
        }
    }
    dist
}

/// World px per travel minute, at zoom 1.
const PX_PER_MIN: f32 = 16.0;
/// World px of the 1 minute ring; radius grows with √minutes, so a ring's
/// area grows with travel time and the busy centre gets room.
const RING: f32 = 50.0;
const REPULSION: f32 = 600.0;
const REPULSION_RANGE: f32 = 120.0;
const SPRING: f32 = 0.05;
const RADIAL: f32 = 0.3;
const CENTERING: f32 = 0.002;
const DAMPING: f32 = 0.6;
const MAX_STEP: f32 = 40.0;
/// Below this speed, world px per step, the layout counts as settled.
const REST: f32 = 0.05;

type V = [f32; 2];

fn add(a: V, b: V) -> V {
    [a[0] + b[0], a[1] + b[1]]
}
fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1]]
}
fn mul(a: V, k: f32) -> V {
    [a[0] * k, a[1] * k]
}
fn len(a: V) -> f32 {
    a[0].hypot(a[1])
}

/// Stop positions from their places, about 2 min/km, centred on the mean.
pub fn seed(net: &Network) -> Vec<V> {
    let n = net.stops.len().max(1) as f32;
    let lat0 = net.stops.iter().map(|s| s.1).sum::<f32>() / n;
    let lon0 = net.stops.iter().map(|s| s.2).sum::<f32>() / n;
    let k = 2.0 * PX_PER_MIN;
    net.stops
        .iter()
        .map(|s| {
            [
                (s.2 - lon0) * lat0.to_radians().cos() * 111.3 * k,
                (lat0 - s.1) * 111.1 * k,
            ]
        })
        .collect()
}

/// One step of the forces: stops repel, lines pull their stops to the travel
/// time apart, and with `radius` each stop is pulled onto its ring around the
/// origin. Returns the fastest stop's speed.
pub fn step(
    pos: &mut [V],
    vel: &mut [V],
    live: &[bool],
    pairs: &[Pair],
    radius: Option<&[f32]>,
) -> f32 {
    let n = pos.len();
    let mut f = vec![[0.0; 2]; n];
    // ponytail: all pairs, O(n²) for ~250 stops; a grid or Barnes-Hut past a few thousand.
    for i in (0..n).filter(|&i| live[i]) {
        for j in (i + 1..n).filter(|&j| live[j]) {
            let d = sub(pos[j], pos[i]);
            let l = len(d).max(1.0);
            if l < REPULSION_RANGE {
                let push = mul(d, REPULSION / (l * l * l));
                f[i] = sub(f[i], push);
                f[j] = add(f[j], push);
            }
        }
    }
    for p in pairs {
        let d = sub(pos[p.b], pos[p.a]);
        let l = len(d).max(0.01);
        let pull = mul(d, SPRING * (l - p.minutes * PX_PER_MIN) / l);
        f[p.a] = add(f[p.a], pull);
        f[p.b] = sub(f[p.b], pull);
    }
    let mut fastest: f32 = 0.0;
    for i in (0..n).filter(|&i| live[i]) {
        match radius {
            // The centre glides to the origin whatever its neighbours pull.
            Some(r) if r[i] == 0.0 => {
                let v = mul(pos[i], -0.2);
                vel[i] = [0.0; 2];
                pos[i] = add(pos[i], v);
                fastest = fastest.max(len(v));
                continue;
            }
            Some(r) => {
                let l = len(pos[i]);
                if l < 0.01 && r[i] > 0.0 {
                    // Off the exact centre, so the pull has a direction.
                    let a = i as f32;
                    pos[i] = [a.cos(), a.sin()];
                }
                let l = len(pos[i]).max(0.01);
                f[i] = add(f[i], mul(pos[i], RADIAL * (r[i] - l) / l));
            }
            None => f[i] = sub(f[i], mul(pos[i], CENTERING)),
        }
        let mut v = mul(add(vel[i], f[i]), DAMPING);
        let s = len(v);
        if s > MAX_STEP {
            v = mul(v, MAX_STEP / s);
        }
        vel[i] = v;
        pos[i] = add(pos[i], v);
        fastest = fastest.max(s.min(MAX_STEP));
    }
    fastest
}

fn ring(minutes: f32) -> f32 {
    RING * minutes.sqrt()
}

/// Ring radius of each stop for its travel time from `center`; stops out
/// of reach go just past the farthest.
pub fn rings(n: usize, pairs: &[Pair], center: usize) -> Vec<f32> {
    let d = distances(n, pairs, center);
    let far = d
        .iter()
        .copied()
        .filter(|m| m.is_finite())
        .fold(0.0, f32::max);
    d.iter()
        .map(|&m| ring(if m.is_finite() { m } else { far + 5.0 }))
        .collect()
}

const LINE_WIDTH: f32 = 3.0;
const LANE: f32 = 3.5;
const DOT: f32 = 4.0;
const TAP: f32 = 24.0;
const SLOP: f32 = 8.0;

#[derive(Default)]
struct Sim {
    pos: Vec<V>,
    vel: Vec<V>,
    /// World offset of the view; eased back to 0 after picking a stop.
    pan: V,
    recenter: bool,
    /// Screen px per world px; follows the layout's extent until zoomed by hand.
    zoom: f32,
    zoomed: bool,
    /// The centre the layout last moved to, to notice a new one.
    center: Option<usize>,
    press: Option<(V, V, bool)>,
    last: Option<Instant>,
}

impl Sim {
    fn scale(&self, w: &NetworkView) -> f32 {
        self.zoom * 1.4f32.powi(w.zoom)
    }
    fn to_screen(&self, w: &NetworkView, r: Rect, p: V) -> V {
        let k = self.scale(w);
        [
            r.x + r.w / 2.0 + (p[0] + self.pan[0]) * k,
            r.y + r.h / 2.0 + (p[1] + self.pan[1]) * k,
        ]
    }
}

/// The network, laid out around `center`; drag to pan, wheel to zoom, tap a
/// stop to pick it (again to let go).
pub struct NetworkView {
    pub hidden: HashSet<String>,
    /// Steps of 1.4× on top of the fitted zoom.
    pub zoom: i32,
    /// Stop index the layout radiates from.
    pub center: Option<usize>,
    pub on_select: OnChange<Option<usize>>,
}

impl From<NetworkView> for Element {
    fn from(w: NetworkView) -> Element {
        Element::new(w)
    }
}

impl Widget for NetworkView {
    fn style(&self) -> Style {
        Style {
            flex_grow: 1.0,
            flex_basis: Dimension::length(0.0),
            min_size: Size::length(0.0),
            ..Style::default()
        }
    }

    fn paint(&self, cx: &mut PaintCx) {
        let net = network();
        let lines = net.lines();
        let pairs = pairs(net, &lines, &self.hidden);
        let n = net.stops.len();
        let mut live = vec![false; n];
        for p in &pairs {
            live[p.a] = true;
            live[p.b] = true;
        }
        let r = cx.rect;
        let now = cx.now;
        let sim: &mut Sim = cx.state.get();
        if sim.pos.len() != n {
            sim.pos = seed(net);
            sim.vel = vec![[0.0; 2]; n];
        }
        let center = self.center.filter(|&c| live[c]);
        if center != sim.center {
            sim.center = center;
            sim.recenter = center.is_some();
        }
        let radius = sim.center.map(|c| rings(n, &pairs, c));
        // Steps of about 8 ms, so the layout moves at the same pace at any frame rate.
        let elapsed = sim
            .last
            .map_or(0.016, |t| now.duration_since(t).as_secs_f32());
        sim.last = Some(now);
        let mut fastest: f32 = 0.0;
        for _ in 0..((elapsed / 0.008).round() as usize).clamp(1, 4) {
            fastest = fastest.max(step(
                &mut sim.pos,
                &mut sim.vel,
                &live,
                &pairs,
                radius.as_deref(),
            ));
        }
        let mut fitting = false;
        if !sim.zoomed {
            let extent = (0..n)
                .filter(|&i| live[i])
                .map(|i| len(sim.pos[i]))
                .fold(1.0, f32::max);
            // Twice the zoom that fits it all: the centre is what matters.
            let fit = r.w.min(r.h) * 0.92 / extent;
            fitting = (fit - sim.zoom).abs() > fit * 0.002;
            sim.zoom = if sim.zoom == 0.0 {
                fit
            } else {
                sim.zoom + (fit - sim.zoom) * 0.2
            };
        }
        if sim.recenter {
            sim.pan = mul(sim.pan, 0.85);
            sim.recenter = len(sim.pan) > 0.5;
        }
        let moving = fastest > REST || sim.recenter || fitting;
        if !moving {
            sim.last = None;
        }

        let sim: &Sim = sim;
        let k = sim.scale(self);
        let at = |i: usize| sim.to_screen(self, r, sim.pos[i]);
        let mut shapes: Vec<Path> = vec![Path::default(); lines.len()];
        for p in &pairs {
            let (a, b) = (at(p.a), at(p.b));
            let d = sub(b, a);
            let l = len(d);
            if l < 0.01 {
                continue;
            }
            let nrm = [-d[1] / l, d[0] / l];
            let lanes = p.lines.len() as f32;
            for (i, &line) in p.lines.iter().enumerate() {
                let o = (i as f32 - (lanes - 1.0) / 2.0) * LANE;
                let off = mul(nrm, o);
                let lane = Path::stroke(&[add(a, off), add(b, off)], Stroke::new(LINE_WIDTH));
                shapes[line].contours.extend(lane.contours);
            }
        }
        let mut served = vec![0usize; n];
        for p in &pairs {
            served[p.a] = served[p.a].max(p.lines.len());
            served[p.b] = served[p.b].max(p.lines.len());
        }
        let center = sim.center;
        let origin = sim.to_screen(self, r, [0.0, 0.0]);
        let far = radius.as_ref().map(|rs| {
            let r = (0..n)
                .filter(|&i| live[i])
                .map(|i| rs[i])
                .fold(0.0, f32::max);
            (r / RING).powi(2)
        });
        let mut labels: Vec<(V, &str, bool, usize)> = (0..n)
            .filter(|&i| live[i] && r.contains(at(i)[0], at(i)[1]))
            .map(|i| (at(i), net.stops[i].0.as_str(), Some(i) == center, served[i]))
            .collect();
        labels.sort_by_key(|l| (!l.2, Reverse(l.3)));
        let dots: Vec<(V, f32, bool)> = (0..n)
            .filter(|&i| live[i])
            .map(|i| {
                (
                    at(i),
                    if served[i] >= 3 { DOT * 1.4 } else { DOT },
                    Some(i) == center,
                )
            })
            .collect();

        cx.with_clip(r, |cx| {
            if let Some(far) = far {
                let every = if far > 40.0 { 10 } else { 5 };
                for m in (every..=far.ceil() as usize).step_by(every) {
                    let rr = ring(m as f32) * k;
                    let ring = Rect::new(origin[0] - rr, origin[1] - rr, 2.0 * rr, 2.0 * rr);
                    cx.fill_bordered(ring, rr, Color::TRANSPARENT, 1.0, MUTED.alpha(0.25));
                    let label = format!("{m} min");
                    cx.text(
                        &label,
                        Font::DEFAULT,
                        11.0,
                        MUTED,
                        None,
                        (origin[0] + 4.0, origin[1] - rr + 2.0),
                    );
                }
            }
            for (i, s) in shapes.iter().enumerate() {
                cx.path(s, line_color(&lines[i]).0);
            }
            for &(p, d, selected) in &dots {
                let d = if selected { d * 1.6 } else { d };
                let dot = Rect::new(p[0] - d, p[1] - d, 2.0 * d, 2.0 * d);
                let fill = if selected { ACCENT } else { Color::WHITE };
                cx.fill_bordered(dot, d, fill, 1.5, INK);
            }
            // Busiest stops first, right of the dot or else left of it; a
            // label that would overlap a placed one is left out, so more
            // show as zooming in makes room.
            // ponytail: O(labels²) overlap test; a grid if it shows up when zoomed out.
            let mut placed: Vec<Rect> = vec![];
            for &(p, name, selected, _) in &labels {
                let (font, size, color) = match selected {
                    true => (Font::BOLD, 15.0, INK),
                    false => (Font::DEFAULT, 11.0, MUTED),
                };
                let t = cx.measure(name, font, size, None);
                let y = p[1] - t.height / 2.0;
                let sides = [p[0] + 9.0, p[0] - 9.0 - t.width];
                let free = sides
                    .map(|x| Rect::new(x, y, t.width, t.height))
                    .into_iter()
                    .find(|at| {
                        !placed.iter().any(|o| {
                            let i = o.intersect(at);
                            i.w > 0.0 && i.h > 0.0
                        })
                    });
                if let Some(at) = free {
                    placed.push(at);
                    cx.text(name, font, size, color, None, (at.x, at.y));
                }
            }
        });
        if moving {
            cx.request_frame();
        }
    }

    fn event(&self, ev: &Event, cx: &mut EventCx) {
        cx.with_state(|sim: &mut Sim, cx| {
            if sim.zoom != 0.0 {
                self.pointer(ev, sim, cx);
            }
        });
    }
}

impl NetworkView {
    /// Pans on a drag, zooms on the wheel and picks a centre on a tap.
    fn pointer(&self, ev: &Event, sim: &mut Sim, cx: &mut EventCx) {
        let r = cx.rect;
        match *ev {
            Event::PointerDown { x, y } => {
                sim.press = Some(([x, y], [x, y], false));
                cx.capture();
            }
            Event::PointerMove { x, y } => {
                let Some((start, last, dragging)) = sim.press else {
                    return;
                };
                let dragging = dragging || len(sub([x, y], start)) > SLOP;
                if dragging {
                    sim.pan = add(sim.pan, mul(sub([x, y], last), 1.0 / sim.scale(self)));
                    sim.recenter = false;
                    cx.request_redraw();
                }
                sim.press = Some((start, [x, y], dragging));
            }
            Event::PointerUp { x, y } => {
                let Some((_, _, false)) = sim.press.take() else {
                    return;
                };
                let net = network();
                let lines = net.lines();
                let pairs = pairs(net, &lines, &self.hidden);
                let tapped = pairs
                    .iter()
                    .flat_map(|p| [p.a, p.b])
                    .map(|i| (i, len(sub(sim.to_screen(self, r, sim.pos[i]), [x, y]))))
                    .filter(|&(_, d)| d < TAP)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(i, _)| i);
                if let Some(i) = tapped {
                    let picked = (self.center != Some(i)).then_some(i);
                    (self.on_select)(cx, picked);
                }
            }
            Event::Cancel | Event::Leave => sim.press = None,
            Event::Wheel { x, y, dy } => {
                let c = [r.x + r.w / 2.0, r.y + r.h / 2.0];
                let k = sim.scale(self);
                let world = sub(mul(sub([x, y], c), 1.0 / k), sim.pan);
                sim.zoom *= (dy * 0.002).exp();
                sim.zoomed = true;
                sim.pan = sub(mul(sub([x, y], c), 1.0 / sim.scale(self)), world);
                sim.recenter = false;
                cx.request_redraw();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forces_put_stops_on_their_rings() {
        let net = network();
        let lines = net.lines();
        let p = pairs(net, &lines, &HashSet::new());
        let center = net
            .stops
            .iter()
            .position(|s| s.0 == "Hauptbahnhof")
            .unwrap();
        let r = rings(net.stops.len(), &p, center);
        let mut pos = seed(net);
        let mut vel = vec![[0.0; 2]; pos.len()];
        let live = vec![true; pos.len()];
        let mut steps = 0;
        while step(&mut pos, &mut vel, &live, &p, Some(&r)) > REST {
            steps += 1;
            assert!(steps < 3000, "doesn't settle");
        }
        assert!(len(pos[center]) < 2.0);
        // Within a minute and a half of the ring, for most stops.
        let off = (0..pos.len())
            .filter(|&i| (len(pos[i]) - r[i]).abs() > 1.5 * PX_PER_MIN)
            .count();
        assert!(off * 10 < pos.len(), "{off} stops off their ring");
    }

    fn pair(a: usize, b: usize, minutes: f32) -> Pair {
        Pair {
            a,
            b,
            minutes,
            lines: vec![0],
        }
    }

    #[test]
    fn distances_take_the_shortest_way() {
        let p = [pair(0, 1, 2.0), pair(1, 2, 2.0), pair(0, 2, 5.0)];
        let d = distances(4, &p, 0);
        assert_eq!(&d[..3], &[0.0, 2.0, 4.0]);
        assert!(d[3].is_infinite());
    }

    #[test]
    fn hidden_lines_drop_their_pairs() {
        let net = Network {
            trips: vec![],
            stops: vec![],
            edges: vec![
                ("1".into(), 0, 1, 2.0),
                ("3".into(), 0, 1, 1.5),
                ("3".into(), 1, 2, 1.0),
            ],
        };
        let lines = net.lines();
        let all = pairs(&net, &lines, &HashSet::new());
        assert_eq!(all.len(), 2);
        assert_eq!((all[0].minutes, all[0].lines.len()), (1.5, 2));
        let hidden = HashSet::from(["3".to_string()]);
        let some = pairs(&net, &lines, &hidden);
        assert_eq!(some.len(), 1);
        assert_eq!(some[0].minutes, 2.0);
    }

    /// Crawls every stop's timetable and rewrites `assets/network.json`.
    #[test]
    #[ignore = "crawls the live API for about a minute"]
    fn refresh_network() {
        use crate::{API, get};
        use std::time::{SystemTime, UNIX_EPOCH};

        #[derive(Deserialize)]
        struct StopEntry {
            stop: GeoStop,
        }
        #[derive(Deserialize)]
        struct GeoStop {
            uid: String,
            name: String,
            lat: String,
            lon: String,
        }
        #[derive(Deserialize)]
        struct Line {
            route: Route,
            trip: Vec<TripStop>,
        }
        #[derive(Deserialize)]
        struct Route {
            name: String,
            direction: String,
        }
        #[derive(Deserialize)]
        struct TripStop {
            name: String,
            #[serde(default)]
            departures: Vec<Dep>,
        }
        #[derive(Deserialize)]
        struct Dep {
            minutes: f64,
            #[serde(rename = "tripID")]
            trip: i64,
        }

        let geo: Vec<GeoStop> = get::<Vec<StopEntry>>(&format!("{API}/STOPS"))
            .unwrap()
            .into_iter()
            .map(|e| e.stop)
            .collect();
        // Minutes are relative to each query, so each answer keeps its clock.
        let clock = || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs_f64()
                / 60.0
        };
        let tables: Vec<(f64, Vec<Line>)> = std::thread::scope(|s| {
            let jobs: Vec<_> = geo
                .chunks(geo.len().div_ceil(8))
                .map(|chunk| {
                    s.spawn(move || {
                        chunk
                            .iter()
                            .filter_map(|g| {
                                let t = clock();
                                let url = format!("{API}/TIMETABLE?stopID={}", g.uid);
                                Some((t, get::<Vec<Line>>(&url).ok()?))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            jobs.into_iter().flat_map(|j| j.join().unwrap()).collect()
        });

        let mut at: HashMap<&str, HashMap<i64, f64>> = HashMap::new();
        for (t, lines) in &tables {
            for s in lines.iter().flat_map(|l| &l.trip) {
                for d in &s.departures {
                    at.entry(&s.name).or_default().insert(d.trip, t + d.minutes);
                }
            }
        }
        let place: HashMap<&str, (f32, f32)> = geo
            .iter()
            .filter_map(|g| Some((g.name.as_str(), (g.lat.parse().ok()?, g.lon.parse().ok()?))))
            .collect();
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut stops = vec![];
        let mut samples: HashMap<(String, usize, usize), Vec<f64>> = HashMap::new();
        let mut trips: HashMap<(String, String), Vec<usize>> = HashMap::new();
        for (_, lines) in &tables {
            for l in lines.iter().filter(|l| l.trip.len() > 1) {
                let mut idx = |n: &String| {
                    *index.entry(n.clone()).or_insert_with(|| {
                        let (lat, lon) = place.get(n.as_str()).copied().unwrap_or_default();
                        stops.push((n.clone(), lat, lon));
                        stops.len() - 1
                    })
                };
                let seq: Vec<usize> = l.trip.iter().map(|t| idx(&t.name)).collect();
                let key = (l.route.name.clone(), l.route.direction.clone());
                let longest = trips.entry(key).or_default();
                if seq.len() > longest.len() {
                    *longest = seq;
                }
                for w in l.trip.windows(2) {
                    let [i, j] = [&w[0].name, &w[1].name].map(&mut idx);
                    if i == j {
                        continue;
                    }
                    let e = samples
                        .entry((l.route.name.clone(), i.min(j), i.max(j)))
                        .or_default();
                    if let (Some(a), Some(b)) =
                        (at.get(w[0].name.as_str()), at.get(w[1].name.as_str()))
                    {
                        e.extend(
                            a.iter()
                                .filter_map(|(trip, ta)| Some(b.get(trip)? - ta))
                                .filter(|m| (0.0..30.0).contains(m)),
                        );
                    }
                }
            }
        }
        let km = |a: usize, b: usize| {
            let ((_, la, oa), (_, lb, ob)) = (&stops[a], &stops[b]);
            if *la == 0.0 || *lb == 0.0 {
                return 1.0;
            }
            let x = (ob - oa) * (la.to_radians().cos()) * 111.3;
            let y = (lb - la) * 111.1;
            (x * x + y * y).sqrt()
        };
        let mut edges: Vec<_> = samples
            .into_iter()
            .map(|((line, a, b), mut m)| {
                m.sort_by(f64::total_cmp);
                // ponytail: about 2 min/km where no trip was seen at both stops.
                let minutes = m.get(m.len() / 2).map_or(km(a, b) * 2.0, |&v| v as f32);
                (line, a, b, (minutes.max(0.3) * 10.0).round() / 10.0)
            })
            .collect();
        edges.sort_by(|x, y| (&x.0, x.1, x.2).cmp(&(&y.0, y.1, y.2)));
        let mut trips: Vec<_> = trips.into_iter().map(|((l, d), s)| (l, d, s)).collect();
        trips.sort();
        let net = Network {
            stops,
            edges,
            trips,
        };
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/network.json");
        std::fs::write(path, serde_json::to_string(&net).unwrap()).unwrap();
        let missing = net.stops.iter().filter(|s| s.1 == 0.0).count();
        println!(
            "{} stops ({missing} without place), {} edges",
            net.stops.len(),
            net.edges.len()
        );
    }
}
