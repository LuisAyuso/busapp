//! Direct and one-change connections from IVB's TIMETABLE endpoint. A trip
//! keeps its `tripID` along the line, so a trip's time at another stop comes
//! from that stop's own timetable.

use std::collections::HashMap;

use serde::Deserialize;

use crate::{API, get};

const MIN_CHANGE: f64 = 1.0;
const MAX_WAIT: f64 = 45.0;
// ponytail: only the busiest change stops are queried; raise if good routes go missing.
const MAX_CHANGE_STOPS: usize = 8;
const MAX_RESULTS: usize = 12;
const MAX_TERMINUS_NEIGHBOURS: usize = 3;
// ponytail: two change stops queried per route; raise if good two-change routes go missing.
const MAX_VIAS: usize = 4;
/// Last-hop time when no outbound trip shows how long it takes.
const LAST_HOP: f64 = 2.0;

#[derive(Deserialize, Clone)]
pub struct Line {
    route: LineInfo,
    trip: Vec<TripStop>,
}

#[derive(Deserialize, Clone)]
struct LineInfo {
    #[serde(rename = "routeID")]
    id: i64,
    name: String,
    direction: String,
}

#[derive(Deserialize, Clone)]
struct TripStop {
    #[serde(rename = "stopID")]
    id: i64,
    name: String,
    #[serde(default)]
    current: bool,
    #[serde(default)]
    departures: Vec<Dep>,
}

#[derive(Deserialize, Clone)]
struct Dep {
    time: String,
    minutes: f64,
    #[serde(rename = "tripID")]
    trip: i64,
    #[serde(default)]
    plabel: String,
    /// Derived from the stop before, see `terminus_arrivals`.
    #[serde(skip)]
    estimated: bool,
}

pub struct Leg {
    pub line: String,
    pub direction: String,
    pub from: String,
    pub platform: String,
    /// Minutes from now.
    pub dep: f64,
    pub arr: f64,
    pub estimated: bool,
}

/// A stop and the minutes on foot between it and the place the trip
/// starts or ends at.
pub type Walk = (String, f64);

pub struct Connection {
    pub legs: Vec<Leg>,
    /// The walk to the first stop.
    pub lead: Option<Walk>,
    /// The walk on from the last stop.
    pub walk: Option<Walk>,
}

impl Connection {
    pub fn dep(&self) -> f64 {
        self.legs[0].dep
    }
    pub fn arr(&self) -> f64 {
        self.legs[self.legs.len() - 1].arr
    }
    /// When to set off, walking to the first stop first.
    pub fn leave(&self) -> f64 {
        self.dep() - self.lead.as_ref().map_or(0.0, |w| w.1)
    }
    /// Arrival at the destination, after any walk.
    pub fn at(&self) -> f64 {
        self.arr() + self.walk.as_ref().map_or(0.0, |w| w.1)
    }
    pub fn estimated(&self) -> bool {
        self.legs[self.legs.len() - 1].estimated
    }
}

pub struct Plan {
    /// Innsbruck time of day in minutes, if the API gave a clock time to derive it from.
    pub now: Option<f64>,
    pub connections: Vec<Connection>,
}

impl Line {
    /// Where the queried stop sits in the trip.
    fn here(&self) -> Option<usize> {
        let t = &self.trip;
        t.iter()
            .position(|t| t.current)
            .or_else(|| t.iter().position(|t| !t.departures.is_empty()))
    }
}

/// Name of the stop a timetable was queried for. Stops are matched by name:
/// regional lines give the same stop a different id than IVB's lines.
fn queried(lines: &[Line]) -> Option<&str> {
    lines
        .iter()
        .find_map(|l| Some(l.trip[l.here()?].name.as_str()))
}

fn timetable(stop: i64) -> Result<Vec<Line>, String> {
    get(&format!("{API}/TIMETABLE?stopID={stop}"))
}

/// Minutes until each trip passes the stop these lines were queried at.
fn times(lines: &[Line]) -> HashMap<i64, &Dep> {
    lines
        .iter()
        .flat_map(|l| &l.trip)
        .flat_map(|t| &t.departures)
        .map(|d| (d.trip, d))
        .collect()
}

fn timetables(stops: impl IntoIterator<Item = i64>) -> Vec<(i64, Vec<Line>)> {
    std::thread::scope(|s| {
        let jobs: Vec<_> = stops
            .into_iter()
            .map(|t| (t, s.spawn(move || timetable(t))))
            .collect();
        jobs.into_iter()
            .filter_map(|(t, j)| Some((t, j.join().ok()?.ok()?)))
            .collect()
    })
}

/// Likely stops just before `b` on lines `to` lacks times for: at a terminus
/// the API lists only departures, so arrivals come from these stops instead.
/// That is the stop after `b` on lines starting there, and the stop before it
/// on lines from the start, which differ when a line ends in a loop.
fn terminus_neighbours(b: &str, from: &[Line], to: &[Line]) -> Vec<i64> {
    let known = |l: &Line| {
        to.iter()
            .any(|t| t.route.id == l.route.id && t.route.direction == l.route.direction)
    };
    let outbound = to
        .iter()
        .filter(|l| l.here() == Some(0))
        .filter_map(|l| Some(l.trip.get(1)?.id));
    let inbound = from.iter().filter(|l| !known(l)).filter_map(|l| {
        let ia = l.here()?;
        let ib = ia + 1 + l.trip[ia + 1..].iter().position(|t| t.name == b)?;
        Some(l.trip[ib - 1].id)
    });
    let mut ids: Vec<i64> = inbound.chain(outbound).collect();
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| seen.insert(*id));
    ids.truncate(MAX_TERMINUS_NEIGHBOURS);
    ids
}

/// Lines through neighbour `p` whose next stop is `b` and that `to` lacks,
/// rewritten as if queried at `b`. Their arrival is the time at `p` plus the
/// `b` → `p` time of the outbound trips, taken to be the same both ways.
fn terminus_arrivals(b: &str, p: i64, to: &[Line], near: &[Line]) -> Vec<Line> {
    let at_p = times(near);
    let hop = to
        .iter()
        .filter(|l| l.here() == Some(0) && l.trip.get(1).is_some_and(|t| t.id == p))
        .flat_map(|l| &l.trip[0].departures)
        .find_map(|d| Some(at_p.get(&d.trip)?.minutes - d.minutes))
        .filter(|&m| m > 0.0)
        .unwrap_or(LAST_HOP);
    let known = |l: &Line| {
        to.iter()
            .any(|t| t.route.id == l.route.id && t.route.direction == l.route.direction)
    };
    near.iter()
        .filter(|l| !known(l))
        .filter_map(|l| {
            let ip = l.here()?;
            if l.trip.get(ip + 1)?.name != b {
                return None;
            }
            let mut l = l.clone();
            let deps = std::mem::take(&mut l.trip[ip].departures);
            l.trip[ip].current = false;
            let at_b = &mut l.trip[ip + 1];
            at_b.current = true;
            at_b.departures = deps
                .into_iter()
                .map(|d| Dep {
                    minutes: d.minutes + hop,
                    estimated: true,
                    ..d
                })
                .collect();
            Some(l)
        })
        .collect()
}

/// Line pairs (index in `from`, index in `to`) and the first stop where they
/// meet, by the id that can be queried there.
fn changes(from: &[Line], to: &[Line]) -> Vec<(usize, usize, i64)> {
    let (Some(a), Some(b)) = (queried(from), queried(to)) else {
        return vec![];
    };
    let mut out = vec![];
    for (i, l1) in from.iter().enumerate() {
        let Some(ia) = l1.here() else { continue };
        let after = &l1.trip[ia + 1..];
        if after.iter().any(|t| t.name == b) {
            continue;
        }
        for (j, l2) in to.iter().enumerate() {
            let Some(jb) = l2.here() else { continue };
            let before: HashMap<&str, i64> = l2.trip[..jb]
                .iter()
                .map(|t| (t.name.as_str(), t.id))
                .collect();
            if l2.route.id == l1.route.id || before.contains_key(a) {
                continue;
            }
            if let Some(t) = after.iter().find(|t| before.contains_key(t.name.as_str())) {
                // IVB's ids are the small ones; regional lines use hashes.
                let id = t.id.min(before[t.name.as_str()]).max(0);
                out.push((i, j, if id == 0 { t.id } else { id }));
            }
        }
    }
    out
}

/// Change stops shared by the most line pairs first.
fn busiest(pairs: &[(usize, usize, i64)]) -> Vec<i64> {
    let mut n: HashMap<i64, usize> = HashMap::new();
    for &(_, _, t) in pairs {
        *n.entry(t).or_default() += 1;
    }
    let mut stops: Vec<_> = n.into_iter().collect();
    stops.sort_by_key(|&(t, c)| (std::cmp::Reverse(c), t));
    stops
        .into_iter()
        .take(MAX_CHANGE_STOPS)
        .map(|(t, _)| t)
        .collect()
}

/// A two-change route: line `i` of `from` to `c1`, the line `mid` (name,
/// direction) on to `c2`, then line `j` of `to` to the destination.
#[derive(Debug, PartialEq)]
struct Via {
    i: usize,
    j: usize,
    mid: (String, String),
    c1: i64,
    c2: i64,
}

/// The two-change routes passing the fewest stops, one per line triple.
/// `mid` lists the lines that can be ridden in between: name, direction and
/// stop names in order, as the network snapshot has them.
fn vias(from: &[Line], to: &[Line], mid: &[(&str, &str, Vec<&str>)]) -> Vec<Via> {
    let (Some(a), Some(b)) = (queried(from), queried(to)) else {
        return vec![];
    };
    let mut found: Vec<(usize, Via)> = vec![];
    for (i, l1) in from.iter().enumerate() {
        let Some(ia) = l1.here() else { continue };
        let after = &l1.trip[ia + 1..];
        if after.iter().any(|t| t.name == b) {
            continue;
        }
        // Stops after `a`: how many stops on, and the id to query there.
        let on: HashMap<&str, (usize, i64)> = after
            .iter()
            .enumerate()
            .rev()
            .map(|(k, t)| (t.name.as_str(), (k + 1, t.id)))
            .collect();
        for (j, l3) in to.iter().enumerate() {
            let Some(jb) = l3.here() else { continue };
            let before = &l3.trip[..jb];
            if l3.route.name == l1.route.name || before.iter().any(|t| t.name == a) {
                continue;
            }
            // Stops before `b`: how many stops to go.
            let left: HashMap<&str, (usize, i64)> = before
                .iter()
                .enumerate()
                .map(|(k, t)| (t.name.as_str(), (jb - k, t.id)))
                .collect();
            for (name, dir, seq) in mid {
                if *name == l1.route.name || *name == l3.route.name {
                    continue;
                }
                // Cheapest boarding so far: stops from `a` minus its place on `seq`.
                let mut board: Option<(isize, i64)> = None;
                let mut best: Option<(usize, i64, i64)> = None;
                for (q, s) in seq.iter().enumerate() {
                    if let (Some((cost, c1)), Some(&(k3, c2))) = (board, left.get(s)) {
                        let hops = (cost + q as isize) as usize + k3;
                        if best.is_none_or(|b| hops < b.0) {
                            best = Some((hops, c1, c2));
                        }
                    }
                    if let Some(&(k1, c1)) = on.get(s) {
                        let cost = k1 as isize - q as isize;
                        if board.is_none_or(|b| cost < b.0) {
                            board = Some((cost, c1));
                        }
                    }
                }
                if let Some((hops, c1, c2)) = best {
                    let mid = (name.to_string(), dir.to_string());
                    found.push((hops, Via { i, j, mid, c1, c2 }));
                }
            }
        }
    }
    found.sort_by_key(|f| f.0);
    found.into_iter().take(MAX_VIAS).map(|f| f.1).collect()
}

/// Fetches the timetables of `a`, `b` and the likely change stops, then plans.
pub fn fetch(a: i64, b: i64) -> Result<Plan, String> {
    let (from, to) = std::thread::scope(|s| {
        let to = s.spawn(|| timetable(b));
        (timetable(a), to.join().unwrap_or(Err("panicked".into())))
    });
    let (from, mut to) = (from?, to?);
    if let Some(b) = queried(&to).map(str::to_owned) {
        let extra: Vec<Line> = timetables(terminus_neighbours(&b, &from, &to))
            .iter()
            .flat_map(|(p, near)| terminus_arrivals(&b, *p, &to, near))
            .collect();
        to.extend(extra);
    }
    let pairs = changes(&from, &to);
    let net = crate::network::network();
    let mid: Vec<_> = net
        .trips
        .iter()
        .map(|(n, d, seq)| {
            let names = seq.iter().map(|&k| net.stops[k].0.as_str()).collect();
            (n.as_str(), d.as_str(), names)
        })
        .collect();
    let vias = vias(&from, &to, &mid);
    let mut stops = busiest(&pairs);
    for c in vias.iter().flat_map(|v| [v.c1, v.c2]) {
        if !stops.contains(&c) {
            stops.push(c);
        }
    }
    let at = timetables(stops).into_iter().collect();
    Ok(plan(&from, &to, &pairs, &vias, &at))
}

fn leg(l: &Line, from: &TripStop, d: &Dep, arr: &Dep) -> Leg {
    Leg {
        line: l.route.name.clone(),
        direction: l.route.direction.clone(),
        from: from.name.clone(),
        platform: d.plabel.clone(),
        dep: d.minutes,
        arr: arr.minutes,
        estimated: arr.estimated,
    }
}

/// The first departure in `deps` that can be caught after arriving at
/// `arr`, with its arrival where `next` looks it up.
fn catch<'a, 'b>(
    deps: &'a [Dep],
    arr: f64,
    next: impl Fn(&Dep) -> Option<&'b Dep>,
) -> Option<(&'a Dep, &'b Dep)> {
    deps.iter()
        .filter(|d| d.minutes >= arr + MIN_CHANGE && d.minutes <= arr + MAX_WAIT)
        .find_map(|d| Some((d, next(d)?)))
}

/// Minutes until `d`'s trip reaches the stop `at` was queried at, if after `d`.
fn arrival<'a>(at: &HashMap<i64, &'a Dep>, d: &Dep) -> Option<&'a Dep> {
    at.get(&d.trip).copied().filter(|a| a.minutes > d.minutes)
}

/// `line`'s entry in a stop's timetable and its stop there.
fn boarding(tt: &[Line], line: impl Fn(&Line) -> bool) -> Option<(&Line, &TripStop)> {
    let l = tt.iter().find(|l| line(l))?;
    Some((l, &l.trip[l.here()?]))
}

fn plan(
    from: &[Line],
    to: &[Line],
    pairs: &[(usize, usize, i64)],
    vias: &[Via],
    at: &HashMap<i64, Vec<Line>>,
) -> Plan {
    let Some(b) = queried(to) else {
        return Plan {
            now: now(from),
            connections: vec![],
        };
    };
    let at_b = times(to);
    let reaches_b = |d: &Dep| arrival(&at_b, d);
    let mut out = vec![];
    for l in from {
        let Some(ia) = l.here() else { continue };
        if !l.trip[ia + 1..].iter().any(|t| t.name == b) {
            continue;
        }
        let sa = &l.trip[ia];
        for d in &sa.departures {
            if let Some(arr) = reaches_b(d) {
                out.push(Connection {
                    legs: vec![leg(l, sa, d, arr)],
                    lead: None,
                    walk: None,
                });
            }
        }
    }
    for &(i, j, t) in pairs {
        let Some(tt) = at.get(&t) else { continue };
        let at_t = times(tt);
        let (l1, l2) = (&from[i], &to[j]);
        let same = |l: &&Line| l.route.id == l2.route.id && l.route.direction == l2.route.direction;
        let Some(l2t) = tt.iter().find(same) else {
            continue;
        };
        let (Some(ia), Some(it)) = (l1.here(), l2t.here()) else {
            continue;
        };
        let (sa, st) = (&l1.trip[ia], &l2t.trip[it]);
        for d1 in &sa.departures {
            let Some(a1) = arrival(&at_t, d1) else {
                continue;
            };
            if let Some((d2, a2)) = catch(&st.departures, a1.minutes, reaches_b) {
                out.push(Connection {
                    legs: vec![leg(l1, sa, d1, a1), leg(l2t, st, d2, a2)],
                    lead: None,
                    walk: None,
                });
            }
        }
    }
    for v in vias {
        let (Some(t1), Some(t2)) = (at.get(&v.c1), at.get(&v.c2)) else {
            continue;
        };
        let (l1, l3) = (&from[v.i], &to[v.j]);
        let m = boarding(t1, |l| {
            l.route.name == v.mid.0 && l.route.direction == v.mid.1
        });
        let n = boarding(t2, |l| {
            l.route.id == l3.route.id && l.route.direction == l3.route.direction
        });
        let (Some(ia), Some((m, s1)), Some((n, s2))) = (l1.here(), m, n) else {
            continue;
        };
        let sa = &l1.trip[ia];
        let (at1, at2) = (times(t1), times(t2));
        for d1 in &sa.departures {
            let Some(a1) = arrival(&at1, d1) else {
                continue;
            };
            let Some((d2, a2)) = catch(&s1.departures, a1.minutes, |d| arrival(&at2, d)) else {
                continue;
            };
            if let Some((d3, a3)) = catch(&s2.departures, a2.minutes, reaches_b) {
                out.push(Connection {
                    legs: vec![leg(l1, sa, d1, a1), leg(m, s1, d2, a2), leg(n, s2, d3, a3)],
                    lead: None,
                    walk: None,
                });
            }
        }
    }
    out.retain(|c| c.dep() >= 0.0);
    best(&mut out);
    Plan {
        now: now(from),
        connections: out,
    }
}

/// Keeps only options no other one beats by leaving later and arriving no
/// later, soonest arrival first.
fn best(out: &mut Vec<Connection>) {
    // Ones that leave before the walk to their first stop could get there go.
    out.retain(|c| c.leave() >= 0.0);
    out.sort_by(|x, y| {
        x.at()
            .total_cmp(&y.at())
            .then(y.leave().total_cmp(&x.leave()))
    });
    let mut latest = f64::NEG_INFINITY;
    out.retain(|c| {
        let keep = c.leave() > latest;
        latest = latest.max(c.leave());
        keep
    });
    out.truncate(MAX_RESULTS);
}

/// A trip to plan: from one stop to another, with the walk to the first
/// and the walk on from the last.
pub type Trip = (i64, i64, Option<Walk>, Option<Walk>);

/// Plans every trip at once and keeps the best by arrival after the walks.
/// Fails only if every plan does.
pub fn fetch_all(trips: &[Trip]) -> Result<Plan, String> {
    let plans: Vec<_> = std::thread::scope(|s| {
        let runs: Vec<_> = (trips.iter())
            .map(|&(a, b, ..)| s.spawn(move || fetch(a, b)))
            .collect();
        runs.into_iter()
            .map(|r| r.join().unwrap_or(Err("panicked".into())))
            .collect()
    });
    let (mut now, mut connections, mut error) = (None, Vec::new(), None);
    for (p, (_, _, lead, walk)) in plans.into_iter().zip(trips) {
        match p {
            Ok(p) => {
                now = now.or(p.now);
                connections.extend(p.connections.into_iter().map(|c| Connection {
                    lead: lead.clone(),
                    walk: walk.clone(),
                    ..c
                }));
            }
            Err(e) => error = Some(e),
        }
    }
    match error {
        Some(e) if now.is_none() && connections.is_empty() => Err(e),
        _ => {
            best(&mut connections);
            Ok(Plan { now, connections })
        }
    }
}

/// Innsbruck time of day from any departure the API gave as a clock time.
fn now(lines: &[Line]) -> Option<f64> {
    lines
        .iter()
        .flat_map(|l| &l.trip)
        .flat_map(|t| &t.departures)
        .find_map(|d| {
            let (h, m) = d.time.split_once(':')?;
            Some(h.parse::<f64>().ok()? * 60.0 + m.parse::<f64>().ok()? - d.minutes)
        })
}

/// `minutes` from now as HH:MM, or relative when the clock is unknown.
pub fn clock(now: Option<f64>, minutes: f64) -> String {
    match now {
        Some(n) => {
            let t = ((n + minutes).round() as i64).rem_euclid(24 * 60);
            format!("{:02}:{:02}", t / 60, t % 60)
        }
        None => format!("+{} min", minutes.round()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: i64, name: &str, stops: &[i64], at: i64, deps: &[(i64, f64)]) -> Line {
        Line {
            route: LineInfo {
                id,
                name: name.into(),
                direction: "x".into(),
            },
            trip: stops
                .iter()
                .map(|&s| TripStop {
                    id: s,
                    name: s.to_string(),
                    current: s == at,
                    departures: if s == at {
                        deps.iter()
                            .map(|&(trip, minutes)| Dep {
                                time: format!("{minutes} min"),
                                minutes,
                                trip,
                                plabel: "A".into(),
                                estimated: false,
                            })
                            .collect()
                    } else {
                        vec![]
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn plans_direct_and_one_change() {
        // Line 1: A(1) → T(5) → C(6). Line 2: T(5) → B(9). Line 3: A(1) → B(9).
        let from = vec![
            line(1, "1", &[1, 5, 6], 1, &[(10, 2.0), (11, 12.0)]),
            line(3, "3", &[1, 9], 1, &[(30, 4.0), (31, 1.0)]),
        ];
        let to = vec![
            line(2, "2", &[5, 9], 9, &[(20, 15.0), (21, 25.0)]),
            // Trip 31 leaves first but arrives last: beaten by every other option.
            line(3, "3", &[1, 9], 9, &[(30, 20.0), (31, 30.0)]),
        ];
        let pairs = changes(&from, &to);
        assert_eq!(pairs, vec![(0, 0, 5)]);
        let at = HashMap::from([(
            5,
            vec![
                line(1, "1", &[1, 5, 6], 5, &[(10, 6.0), (11, 16.0)]),
                line(2, "2", &[5, 9], 5, &[(20, 8.0), (21, 18.0)]),
            ],
        )]);
        let p = plan(&from, &to, &pairs, &[], &at);
        let got: Vec<_> = p
            .connections
            .iter()
            .map(|c| {
                (
                    c.legs.iter().map(|l| l.line.as_str()).collect::<Vec<_>>(),
                    c.dep(),
                    c.arr(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (vec!["1", "2"], 2.0, 15.0),
                (vec!["3"], 4.0, 20.0),
                (vec!["1", "2"], 12.0, 25.0),
            ]
        );
    }

    #[test]
    fn plans_two_changes_through_a_middle_line() {
        // Line 1: A(1) → X(5). Line 7 (only in the snapshot): 4 → X(5) → Y(6).
        // Line 3: Y(6) → B(9). No line joins 1 and 3 directly.
        let from = vec![line(1, "1", &[1, 5], 1, &[(10, 2.0)])];
        let to = vec![line(3, "3", &[6, 9], 9, &[(30, 20.0)])];
        let mid = [("7", "x", vec!["4", "5", "6"])];
        let v = vias(&from, &to, &mid);
        assert_eq!(
            v,
            vec![Via {
                i: 0,
                j: 0,
                mid: ("7".into(), "x".into()),
                c1: 5,
                c2: 6
            }]
        );
        assert!(changes(&from, &to).is_empty());
        let at = HashMap::from([
            (
                5,
                vec![
                    line(1, "1", &[1, 5], 5, &[(10, 6.0)]),
                    line(7, "7", &[4, 5, 6], 5, &[(70, 8.0)]),
                ],
            ),
            (
                6,
                vec![
                    line(7, "7", &[4, 5, 6], 6, &[(70, 11.0)]),
                    line(3, "3", &[6, 9], 6, &[(30, 14.0)]),
                ],
            ),
        ]);
        let p = plan(&from, &to, &[], &v, &at);
        let c = &p.connections[0];
        let lines: Vec<_> = c.legs.iter().map(|l| l.line.as_str()).collect();
        assert_eq!((lines, c.dep(), c.arr()), (vec!["1", "7", "3"], 2.0, 20.0));
    }

    #[test]
    fn estimates_arrival_at_terminus() {
        // B(9) is a terminus: `to` only has line 4 leaving it via P(8),
        // 2 min from B on trip 40. Line 5 runs A(7) → P(8) → B(9).
        let mut to = vec![line(4, "4", &[9, 8, 7], 9, &[(40, 3.0)])];

        let near = vec![
            line(4, "4", &[9, 8, 7], 8, &[(40, 5.0)]),
            line(5, "5", &[7, 8, 9], 8, &[(50, 10.0)]),
        ];
        to.extend(terminus_arrivals("9", 8, &to, &near));
        let from = vec![line(5, "5", &[7, 8, 9], 7, &[(50, 6.0)])];
        // Line 6 ends in a loop: it reaches B(9) from 3, not from 8.
        let loops = line(6, "6", &[7, 3, 9], 7, &[]);
        assert_eq!(terminus_neighbours("9", &[loops], &to), vec![3, 8]);
        let p = plan(&from, &to, &[], &[], &HashMap::new());
        let c = &p.connections[0];
        assert_eq!((c.dep(), c.arr(), c.estimated()), (6.0, 12.0, true));
    }

    #[test]
    fn best_ranks_by_arrival_after_the_walk() {
        let trip = |dep: f64, arr: f64, walk: f64| Connection {
            legs: vec![Leg {
                line: "1".into(),
                direction: "x".into(),
                from: "a".into(),
                platform: "A".into(),
                dep,
                arr,
                estimated: false,
            }],
            lead: None,
            walk: Some(("b".into(), walk)),
        };
        // Arrives first by bus but walks longest; the one leaving earlier and
        // arriving later after its walk is beaten and goes.
        let mut out = vec![
            trip(2.0, 10.0, 8.0),
            trip(3.0, 12.0, 1.0),
            trip(1.0, 9.0, 9.0),
        ];
        best(&mut out);
        let at: Vec<f64> = out.iter().map(Connection::at).collect();
        assert_eq!(at, [13.0]);
    }

    #[test]
    fn best_drops_buses_gone_before_the_walk_there() {
        let trip = |dep: f64, arr: f64| Connection {
            legs: vec![Leg {
                line: "1".into(),
                direction: "x".into(),
                from: "a".into(),
                platform: "A".into(),
                dep,
                arr,
                estimated: false,
            }],
            lead: Some(("a".into(), 5.0)),
            walk: None,
        };
        let mut out = vec![trip(3.0, 8.0), trip(6.0, 12.0), trip(9.0, 13.0)];
        best(&mut out);
        let leave: Vec<f64> = out.iter().map(Connection::leave).collect();
        assert_eq!(leave, [1.0, 4.0]);
    }

    #[test]
    fn clock_from_server_time() {
        let mut l = line(1, "1", &[1], 1, &[(1, 60.5)]);
        l.trip[0].departures[0].time = "23:18".into();
        let n = now(&[l]);
        assert_eq!(clock(n, 60.5), "23:18");
        assert_eq!(clock(n, 120.0), "00:18");
        assert_eq!(clock(None, 4.4), "+4 min");
    }
}
