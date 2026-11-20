//! Lines as a list, and one line as its stops along a rail in its color.

use dfui::taffy::{AlignItems, Dimension, JustifyContent, LengthPercentageAuto, Size, Style};
use dfui::widgets::{Styled, button, column, dropdown, row, scroll, text};
use dfui::{Color, Element, Handle, PaintCx, Rect, Widget};

use crate::network::network;
use crate::{INK, MUTED, Stop, Ui, badge, badges, line_color, sort_lines};

const RAIL: f32 = 6.0;
const DOT: f32 = 14.0;
/// Where a row's dot sits: on its stop name's line.
const DOT_Y: f32 = 19.0;

/// Directions of `line`, longest run first, with their stops.
fn directions(line: &str) -> Vec<(&'static str, &'static [usize])> {
    let mut d: Vec<_> = network()
        .trips
        .iter()
        .filter(|t| t.0 == line)
        .map(|t| (t.1.as_str(), t.2.as_slice()))
        .collect();
    d.sort_by_key(|t| std::cmp::Reverse(t.1.len()));
    d
}

/// Lines serving stop `i`, besides `except`.
fn lines_at(i: usize, except: &str) -> Vec<String> {
    sort_lines(
        network()
            .edges
            .iter()
            .filter(|e| (e.1 == i || e.2 == i) && e.0 != except)
            .map(|e| e.0.clone())
            .collect(),
    )
}

pub fn all(h: Handle<Ui>) -> Element {
    let rows = network().lines().into_iter().map(move |l| {
        let ends: Vec<&str> = directions(&l).iter().map(|d| d.0).collect();
        let label = row([
            badge(&l, 16.0).padding_xy(8.0, 4.0).min_width(48.0).into(),
            text(ends.join(" · "))
                .size(14.0)
                .color(MUTED)
                .shrink(1.0)
                .into(),
        ])
        .align(AlignItems::CENTER)
        .gap(12.0);
        button(label)
            .flat()
            .color(Color::WHITE)
            .justify(JustifyContent::START)
            .on_click(move |cx| {
                let l = l.clone();
                cx.update(h, move |u: &mut Ui| {
                    u.line = Some(l);
                    u.dir = 0;
                })
            })
            .into()
    });
    scroll(column(rows).gap(2.0)).grow(1.0).into()
}

/// `line`'s stops in direction `dir`, each with the lines to change to;
/// tapping one opens its departures.
pub fn line(h: Handle<Ui>, stops: &[Stop], line: &str, dir: usize) -> [Element; 2] {
    let dirs = directions(line);
    let Some(&(_, seq)) = dirs.get(dir).or(dirs.first()) else {
        return [crate::message("Keine Fahrten bekannt."), column([]).into()];
    };
    let color = line_color(line).0;
    let names = &network().stops;
    let rows = seq.iter().enumerate().map(|(k, &i)| {
        let name = &names[i].0;
        let stop = stops.iter().find(|s| &s.name == name).cloned();
        let mut info = vec![text(name).size(16.0).color(INK).into()];
        let other = lines_at(i, line);
        if !other.is_empty() {
            info.push(badges(&other));
        }
        let body = row([
            Rail {
                color,
                first: k == 0,
                last: k + 1 == seq.len(),
            }
            .into(),
            column(info)
                .gap(4.0)
                .padding_xy(0.0, 8.0)
                .grow(1.0)
                .shrink(1.0)
                .into(),
        ])
        .gap(12.0);
        button(body)
            .flat()
            .color(crate::BG)
            .radius(0.0)
            .padding_xy(4.0, 0.0)
            .justify(JustifyContent::START)
            .on_click(move |cx| {
                if let Some(s) = stop.clone() {
                    cx.update(h, move |u: &mut Ui| u.stop = Some(s));
                }
            })
            .into()
    });
    let picker = dropdown(dirs.iter().map(|d| format!("Richtung {}", d.0)), dir)
        .on_change(move |cx, i| cx.update(h, move |u: &mut Ui| u.dir = i));
    [picker.into(), scroll(column(rows)).grow(1.0).into()]
}

/// A stretch of a line's rail with the stop's dot on it.
struct Rail {
    color: Color,
    first: bool,
    last: bool,
}

impl From<Rail> for Element {
    fn from(r: Rail) -> Element {
        Element::new(r)
    }
}

impl Widget for Rail {
    fn style(&self) -> Style {
        Style {
            size: Size {
                width: Dimension::length(DOT + 8.0),
                height: Dimension::auto(),
            },
            min_size: Size {
                width: LengthPercentageAuto::length(DOT + 8.0),
                height: LengthPercentageAuto::length(2.0 * DOT_Y),
            },
            ..Style::default()
        }
    }

    fn paint(&self, cx: &mut PaintCx) {
        let r = cx.rect;
        let x = r.x + r.w / 2.0;
        let dot = r.y + DOT_Y;
        let top = if self.first { dot } else { r.y };
        let bottom = if self.last { dot } else { r.y + r.h };
        cx.fill(
            Rect::new(x - RAIL / 2.0, top, RAIL, bottom - top),
            0.0,
            self.color,
        );
        let d = Rect::new(x - DOT / 2.0, dot - DOT / 2.0, DOT, DOT);
        cx.fill_bordered(d, DOT / 2.0, Color::WHITE, 3.5, self.color);
    }
}
