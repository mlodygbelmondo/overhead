//! The answer as most people need it: one sentence, the coordinates (copy,
//! Google Maps, Street View, OpenStreetMap), and on the map one pin with its
//! uncertainty, the possible area when unsure, the aircraft heard, and a
//! legend that names every mark.

use super::*;
use crate::location::LocationCandidate;
use crate::map::{fmt_chance, pin_label, place_number};
use overhead_adsb::links;
use gpui::{ClipboardItem, size};

/// Most other places drawn without asking: more than this and they wait
/// behind the "Other places" switch.
const OTHERS_AUTO_MAX: usize = 3;

/// A mark in the legend, drawn the way the map draws it.
#[derive(Clone, Copy)]
enum Glyph {
    Pin,
    Compare,
    Circle,
    Area,
    Other,
    Probe,
    Track,
    Heard,
    Plane,
    Search,
    Region,
}

fn glyph(kind: Glyph, t: Theme) -> impl IntoElement {
    canvas(|_, _, _| {}, move |b, _, window, _| {
        let c = point(b.left() + b.size.width / 2.0, b.top() + b.size.height / 2.0);
        let halo = t.map_bg.opacity(0.9);
        match kind {
            Glyph::Pin => map::pin(window, point(c.x, c.y + px(7.)), t.accent, gpui::white(), 0.55),
            Glyph::Compare => map::pin(window, point(c.x, c.y + px(7.)), t.alt, gpui::white(), 0.55),
            Glyph::Circle => crate::draw::circle(window, c, 6.5, Some(t.accent.opacity(0.16)), Some((1.5, t.accent.opacity(0.9)))),
            Glyph::Area => crate::draw::rect(window, Bounds::new(point(c.x - px(7.), c.y - px(5.)), size(px(14.), px(10.))), t.warn.opacity(0.4)),
            Glyph::Other => {
                crate::draw::circle(window, c, 5.5, Some(halo.opacity(0.7)), Some((1.5, t.text_muted)));
                crate::draw::circle(window, c, 1.8, Some(t.text_muted), None);
            }
            Glyph::Probe => {
                crate::draw::circle(window, c, 5.0, None, Some((1.5, t.ok)));
                crate::draw::vline(window, c.x, c.y - px(7.), c.y + px(7.), t.ok);
                crate::draw::hline(window, c.x - px(7.), c.x + px(7.), c.y, t.ok);
            }
            Glyph::Track => crate::draw::polyline(window, &[point(c.x - px(7.), c.y + px(4.)), point(c.x, c.y - px(1.)), point(c.x + px(7.), c.y - px(4.))], 1.4, t.text.opacity(0.6)),
            Glyph::Heard => crate::draw::circle(window, c, 3.5, Some(t.text.opacity(0.85)), Some((1.5, halo))),
            Glyph::Plane => map::plane(window, c, 5.0, t.text, halo),
            Glyph::Search => crate::draw::frame(window, Bounds::new(point(c.x - px(6.), c.y - px(5.)), size(px(12.), px(10.))), t.text_muted.opacity(0.8)),
            Glyph::Region => crate::draw::round_rect(window, Bounds::new(point(c.x - px(6.), c.y - px(5.)), size(px(12.), px(10.))), 2.0, t.accent.opacity(0.06), Some(t.accent.opacity(0.6))),
        }
    })
    .w(px(16.))
    .h(px(16.))
    .flex_none()
}

/// A small text button with an optional icon (links, copy, map controls).
pub(super) fn link_button(t: &Theme, id: impl Into<gpui::ElementId>, label: &str, ic: Option<Icon>) -> gpui::Stateful<gpui::Div> {
    let hover = t.hover;
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(theme::SP1))
        .h(px(theme::CONTROL_H))
        .px(px(theme::SP2))
        .rounded(px(theme::RADIUS))
        .whitespace_nowrap()
        .text_size(px(theme::TEXT_SM))
        .text_color(t.text)
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .children(ic.map(|i| icon(i, 12.0, t.text_muted)))
        .child(label.to_string())
}

/// A square icon button on the map (zoom in / out).
fn map_icon_button(t: &Theme, id: &'static str, ic: Icon) -> gpui::Stateful<gpui::Div> {
    let hover = t.hover;
    div()
        .id(id)
        .size(px(theme::CONTROL_H))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS))
        .bg(t.panel_bg.opacity(0.92))
        .border_1()
        .border_color(t.border)
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(icon(ic, 12.0, t.text))
}

impl Workspace {
    /// Are the other possible places drawn on the map?
    pub(super) fn others_shown(&self, r: &LocationResults) -> bool {
        self.show_others.unwrap_or_else(|| r.ambiguous().is_some() && r.places().len().saturating_sub(1) <= OTHERS_AUTO_MAX)
    }

    pub(super) fn copy_place(&mut self, lat: f64, lon: f64, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(format!("{lat:.5}, {lon:.5}")));
        log::info!("copied {lat:.5}, {lon:.5}");
        self.copied = Some(Instant::now());
        // the button says "Copied" for a moment
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1600)).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
        cx.notify();
    }

    fn open_link(&self, url: &str, cx: &mut Context<Self>) {
        log::info!("open {url}");
        cx.open_url(url);
    }

    /// The verdict in one sentence, with the place's coordinates and links.
    pub(super) fn render_result_card(&mut self, t: &Theme, r: &LocationResults, cx: &mut Context<Self>) -> Option<AnyElement> {
        let best = r.candidates.first()?;
        let places = r.places();
        let sel = self.loc_selected.min(r.candidates.len() - 1);
        let c: &LocationCandidate = &r.candidates[sel];
        let amb = r.ambiguous();
        let n_det = self.detections.len();
        let fits = |m: usize, there: &str| {
            if m >= n_det || n_det == 0 {
                format!("all {m} aircraft heard fit {there}")
            } else {
                format!("{m} of the {n_det} fly-bys fit aircraft that passed {there}")
            }
        };
        let (color, sentence) = if r.is_sample {
            (t.warn, "Sample data: invented places, not a result.".to_string())
        } else if sel != 0 {
            // another place is selected: the sentence is about that place
            let name = pin_label(r, &places, sel);
            // "Near place 1": a candidate within 2 km of a better one
            let name = match name.strip_prefix("Near ") {
                Some(rest) => format!("a spot near {}", lower_first(rest)),
                None => lower_first(&name),
            };
            (t.text_muted, format!("You are looking at {name}: {}. The best place fits better.", fits(c.matched, "there")))
        } else if let Some(a) = amb {
            // what the shaded area holds, as the drawer and the details say
            let shaded: f64 = a.cells.iter().map(|c| c.probability).sum::<f64>().clamp(0.0, 1.0);
            let s = if shaded < 0.5 {
                format!(
                    "Not found for sure: the shaded spots fit best, but together they have only a {} chance. It may well be somewhere else in the area searched.",
                    fmt_chance(shaded)
                )
            } else if a.similar_places > 1 {
                format!("Not sure: {} places fit about equally well. It is most likely ({}) somewhere in the shaded area.", a.similar_places, fmt_chance(shaded))
            } else {
                format!(
                    "Not sure: the evidence is weak. The best guess has a {} chance; the shaded area holds {}.",
                    fmt_chance(best.probability.unwrap_or(a.top1)),
                    fmt_chance(shaded)
                )
            };
            (t.warn, s)
        } else {
            (t.ok, format!("Recorded here, give or take {}: {}.", fmt_dist_short(best.radius_m), fits(best.matched, "here")))
        };
        let label = pin_label(r, &places, sel);
        let (lat, lon) = (c.lat, c.lon);
        let copied = self.copied.is_some_and(|t0| t0.elapsed() < Duration::from_millis(1500));
        let analysis_open = self.analysis.is_some();
        let has_session = self.explain.is_some();
        let l = links::for_place(lat, lon);
        let (g, sv, osm) = (l.google_maps.clone(), l.street_view.clone(), l.openstreetmap.clone());
        let coords = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(theme::SP1))
            // the sentence already says "Recorded here" for a sure answer
            .when(amb.is_some() || r.is_sample || sel != 0, |d| {
                d.child(div().flex_none().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).mr(px(theme::SP1)).child(label))
            })
            .child(mono(format!("{lat:.5}, {lon:.5}")).flex_none().text_size(px(theme::TEXT_SM)).text_color(t.text))
            .child(mono(format!("±{}", fmt_dist_short(c.radius_m))).flex_none().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).mr(px(theme::SP1)))
            .child(
                link_button(t, "copy-coords", if copied { "Copied" } else { "Copy" }, Some(if copied { Icon::Check } else { Icon::Copy }))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.copy_place(lat, lon, cx))),
            )
            .child(div().w(px(1.)).h(px(14.)).flex_none().bg(t.border).mx(px(theme::SP1)))
            .child(link_button(t, "open-gmaps", "Google Maps", Some(Icon::External)).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_link(&g, cx))))
            .child(link_button(t, "open-street", "Street View", Some(Icon::External)).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_link(&sv, cx))))
            .child(link_button(t, "open-osm", "OpenStreetMap", Some(Icon::External)).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_link(&osm, cx))))
            // another place is selected: the way back to the answer
            .when(sel != 0, |d| {
                d.child(div().w(px(1.)).h(px(14.)).flex_none().bg(t.border).mx(px(theme::SP1))).child(
                    link_button(t, "back-to-best", "Back to the best place", Some(Icon::Target)).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.loc_selected = 0;
                        if this.compare == Some(0) {
                            this.compare = None;
                        }
                        this.map_view = None;
                        cx.notify();
                    })),
                )
            });
        Some(
            div()
                .mx(px(theme::PANEL_PX))
                .mb(px(theme::SP2))
                .px(px(theme::SP3))
                .py(px(theme::SP2))
                .rounded(px(theme::RADIUS))
                .border_1()
                .border_color(color.opacity(0.45))
                .bg(color.opacity(0.08))
                .flex()
                .flex_col()
                .gap(px(theme::SP1))
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(theme::SP2))
                        .child(div().flex_none().mt(px(5.)).size(px(8.)).rounded(px(4.)).bg(color))
                        .child(div().flex_1().min_w(px(0.)).text_size(px(theme::TEXT)).text_color(t.text).child(sentence))
                        .when(has_session && !r.is_sample, |d| {
                            d.child(
                                button(t, "analysis-btn", if analysis_open { "Close analysis" } else { "Analysis" }, Some(Icon::Chart), false, true)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_analysis(cx))),
                            )
                        }),
                )
                .child(coords)
                .when_some(amb.filter(|_| self.details), |d, a| {
                    d.child(
                        div()
                            .text_size(px(theme::TEXT_XS))
                            .text_color(t.text_muted)
                            .child(format!("Best place {} · best 10 places together {} · somewhere else {}", fmt_chance(a.top1), fmt_chance(a.top10), fmt_chance(a.elsewhere))),
                    )
                })
                .into_any_element(),
        )
    }

    /// Buttons over the map: framing, the other places, zoom, the legend.
    pub(super) fn render_map_overlays(&mut self, t: &Theme, r: Option<&LocationResults>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out: Vec<AnyElement> = vec![];
        let stop = |d: gpui::Div| d.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let has = r.is_some_and(|r| !r.candidates.is_empty());
        let fit = self.map_fit;
        let user_view = self.map_view.is_some();
        let amb = r.and_then(|r| r.ambiguous()).is_some();
        // a small map (the side panel, a narrow window) gets short labels
        let mb = self.map_rect.get();
        let (mw, _) = (f32::from(mb.size.width), f32::from(mb.size.height));
        let compact = mw > 0.0 && mw < 640.0;
        let narrow = mw > 0.0 && mw < 470.0;
        if has {
            let framing = div()
                .flex()
                .gap(px(2.))
                .p(px(2.))
                .rounded(px(theme::RADIUS))
                .bg(t.panel_bg.opacity(0.92))
                .border_1()
                .border_color(t.border)
                .child(
                    toggle(t, "fit-cand", "", fit == MapFit::Candidate && !user_view)
                        .gap(px(theme::SP1))
                        .child(icon(Icon::Target, 12.0, t.text))
                        .child(if compact { "Result" } else { "Zoom to result" })
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.map_fit = MapFit::Candidate;
                            this.map_view = None;
                            cx.notify();
                        })),
                )
                .when(amb, |d| {
                    d.child(toggle(t, "fit-plaus", if compact { "Possible" } else { "Possible area" }, fit == MapFit::Plausible && !user_view).on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| {
                            this.map_fit = MapFit::Plausible;
                            this.map_view = None;
                            cx.notify();
                        },
                    )))
                })
                .child(toggle(t, "fit-area", if compact { "Search" } else { "Search area" }, fit == MapFit::Area && !user_view).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.map_fit = MapFit::Area;
                    this.map_view = None;
                    cx.notify();
                })));
            out.push(stop(div().absolute().top(px(theme::SP2)).left(px(theme::SP2)).child(framing)).into_any_element());
        }
        // top right: other places, zoom
        let n_other = r.map_or(0, |r| r.places().len().saturating_sub(1));
        let others_on = r.is_some_and(|r| self.others_shown(r));
        let others = (has && n_other > 0).then(|| {
            div()
                .rounded(px(theme::RADIUS))
                .bg(t.panel_bg.opacity(0.92))
                .border_1()
                .border_color(t.border)
                .child(
                    switch(t, "others-btn", &if compact { format!("Other places ({n_other})") } else { format!("Other possible places ({n_other})") }, others_on).on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.show_others = Some(!others_on);
                            cx.notify();
                        }),
                    ),
                )
        });
        // too narrow for one row: the switch goes under the zoom buttons
        let (others_inline, others_below) = if narrow { (None, others) } else { (others, None) };
        let zoom = div()
            .flex()
            .gap(px(theme::SP1))
            .child(map_icon_button(t, "map-zoom-in", Icon::Plus).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.zoom_map(0.75);
                cx.notify();
            })))
            .child(map_icon_button(t, "map-zoom-out", Icon::Minus).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.zoom_map(-0.75);
                cx.notify();
            })));
        let right = div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(theme::SP1))
            .child(div().flex().items_center().gap(px(theme::SP1)).children(others_inline).child(zoom))
            .children(others_below);
        out.push(stop(div().absolute().top(px(theme::SP2)).right(px(theme::SP2)).child(right)).into_any_element());
        if let Some(l) = self.render_legend(t, r, cx) {
            out.push(stop(div().absolute().left(px(theme::SP2)).bottom(px(22.)).child(l)).into_any_element());
        }
        out
    }

    /// Zoom the map about its centre by `dz` zoom levels.
    pub(super) fn zoom_map(&mut self, dz: f64) {
        let b = self.map_rect.get();
        let v = self.map_view_for(f32::from(b.size.width) as f64, f32::from(b.size.height) as f64);
        self.map_view = Some(MapView { zoom: (v.zoom + dz).clamp(1.0, map::MAX_ZOOM), ..v });
    }

    /// Every mark on the map, named.
    fn render_legend(&self, t: &Theme, r: Option<&LocationResults>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let mut items: Vec<(Glyph, String)> = vec![];
        match r {
            Some(r) if !r.candidates.is_empty() => {
                let amb = r.ambiguous().is_some();
                let sel = self.loc_selected;
                let best_sel = place_number(&r.places(), sel) == Some(1);
                items.push((
                    Glyph::Pin,
                    match (amb, best_sel) {
                        (false, true) => "Where it was recorded".into(),
                        (true, true) => "Best guess".into(),
                        _ => "The place you selected".into(),
                    },
                ));
                items.push((Glyph::Circle, if amb { "If it is there: within this circle".into() } else { "Likely within this circle".into() }));
                if amb {
                    items.push((Glyph::Area, "Possible area (darker: more likely)".into()));
                }
                if self.others_shown(r) && r.places().len() > 1 {
                    items.push((Glyph::Other, "Other possible place, its chance".into()));
                }
                if let Some(k) = self.compare.filter(|&k| k != sel && k < r.candidates.len()) {
                    let n = place_number(&r.places(), k).map_or("a place".to_string(), |n| format!("place {n}"));
                    items.push((Glyph::Compare, format!("Compared with {n}")));
                }
                if self.explain.as_ref().and_then(|e| e.probe_at).is_some() {
                    items.push((Glyph::Probe, "Place you are testing".into()));
                }
                let tracks = r.candidates.get(sel).is_some_and(|c| !c.tracks.is_empty());
                if tracks {
                    items.push((Glyph::Track, "Aircraft heard (its flight)".into()));
                    items.push((Glyph::Heard, "Where it was when you heard it".into()));
                    if let Some(ct) = self.cursor_t {
                        items.push((Glyph::Plane, format!("Where it was at {}", crate::draw::fmt_time(ct, 1.0))));
                    }
                }
                if r.search_box.is_some() {
                    items.push((Glyph::Search, "Area searched".into()));
                }
            }
            _ => {
                if self.region_preview.is_some() {
                    items.push((Glyph::Region, "Your region (from the form)".into()));
                }
            }
        }
        if items.is_empty() && self.heat.is_none() {
            return None;
        }
        // open by default only where it leaves room for the map
        let mb = self.map_rect.get();
        let open = self.legend_open.unwrap_or(f32::from(mb.size.width) >= 560.0 && f32::from(mb.size.height) >= 400.0);
        let hover = t.hover;
        let header = div()
            .id("legend-toggle")
            .flex()
            .items_center()
            .gap(px(theme::SP1))
            .h(px(26.))
            .px(px(theme::SP2))
            .cursor_pointer()
            .rounded(px(theme::RADIUS))
            .hover(move |s| s.bg(hover))
            .text_size(px(theme::TEXT_XS))
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.text_muted)
            .child("Legend")
            .child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 10.0, t.text_muted))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.legend_open = Some(!open);
                cx.notify();
            }));
        let heat = self.heat.as_ref().filter(|_| open).map(|h| {
            let name = self.heat_label(h.key);
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(px(theme::SP2))
                .pt(px(theme::SP1))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(t.text).child(name))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(theme::SP1))
                        .text_size(px(theme::TEXT_XS))
                        .text_color(t.text_muted)
                        .child("fits worse")
                        .child(heat_scale())
                        .child("better"),
                )
        });
        Some(
            div()
                .flex()
                .flex_col()
                .pb(px(theme::SP1))
                .rounded(px(theme::RADIUS))
                .bg(t.panel_bg.opacity(0.92))
                .border_1()
                .border_color(t.border)
                .max_w(px(280.))
                .child(header)
                .when(open, |d| {
                    d.children(items.into_iter().map(|(g, text)| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(theme::SP2))
                            .h(px(20.))
                            .px(px(theme::SP2))
                            .text_size(px(theme::TEXT_XS))
                            .text_color(t.text)
                            .child(glyph(g, *t))
                            .child(div().whitespace_nowrap().child(text))
                    }))
                })
                .children(heat)
                .into_any_element(),
        )
    }

    /// A click on the map (no drag): select the place under the pointer.
    pub(super) fn map_click(&mut self, pos: Point<Pixels>) -> bool {
        let LocState::Done(r) = &self.location else { return false };
        let b = self.map_rect.get();
        let v = self.map_view_for(f32::from(b.size.width) as f64, f32::from(b.size.height) as f64);
        let others = self.others_shown(r);
        let near = |c: &LocationCandidate, extra: f32| {
            let p = v.xy(&b, c.lat, c.lon);
            let (dx, dy) = (f32::from(p.x - pos.x), f32::from(p.y - pos.y));
            // the pin's head stands 15 px above its point
            dx.hypot(dy) < 9.0 || (extra > 0.0 && dx.hypot(dy + extra) < 10.0)
        };
        let sel = self.loc_selected;
        let hit = r.places().into_iter().find(|&k| k != sel && (others || Some(k) == self.compare) && r.candidates.get(k).is_some_and(|c| near(c, 15.0)));
        if let Some(k) = hit {
            self.loc_selected = k;
            if self.map_fit == MapFit::Candidate {
                self.map_view = Some(v);
            }
            log::info!("map: selected place {}", place_number(&r.places(), k).unwrap_or(0));
            return true;
        }
        false
    }
}

/// "Place 2 · 1 %" → "place 2 · 1 %" inside a sentence.
fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

/// The score map colours, low to high.
fn heat_scale() -> impl IntoElement {
    canvas(|_, _, _| {}, |b, _, window, _| {
        let lut = overhead_core::colormap::lut();
        let n = 24;
        let w = b.size.width / n as f32;
        for i in 0..n {
            let x = i as f32 / (n - 1) as f32;
            let [r, g, bl] = lut[(x * 255.0) as usize];
            let col: Hsla = gpui::Rgba { r: r as f32 / 255.0, g: g as f32 / 255.0, b: bl as f32 / 255.0, a: 0.4 + 0.6 * x }.into();
            crate::draw::rect(window, Bounds::new(point(b.left() + w * i as f32, b.top()), size(w + px(0.5), b.size.height)), col);
        }
    })
    .w(px(90.))
    .h(px(8.))
    .flex_none()
}
