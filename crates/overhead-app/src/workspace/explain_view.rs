//! The analysis drawer: why the search chose its answer, as the reference
//! video shows it. Places (pick one, compare two: their modelled curves on
//! the spectrogram; right-click the map to test any place), heat maps on the
//! map (each fly-by alone and all together), the per-fly-by scores of every
//! refined place grouped by flight pair, the aircraft a place would have
//! heard (silence), and the time offsets between the fly-bys.

use super::*;
use crate::explain_data::Want;
use crate::heat::{Layer, Scope};
use crate::map::{fmt_chance, place_number};
use crate::spectro::ModelOverlay;
use overhead_adsb::explain::{EventPoint, ModelCurve};
use gpui::size;

/// Colours of flight pairs in the scatter (readable on both themes).
const PAIR_COLORS: [u32; 8] = [0x4e9be6, 0xe0884a, 0x5bb56a, 0xd65c8f, 0x9b7be0, 0xc9a83a, 0x4fb8b0, 0x9aa0a8];

fn pair_color(i: usize) -> Hsla {
    gpui::rgb(PAIR_COLORS[i % PAIR_COLORS.len()]).into()
}

/// One cell of a table row (fixed width, or the rest when `w` is 0).
fn cell(w: f32, s: impl Into<SharedString>) -> gpui::Div {
    let d = div().px(px(theme::SP1)).whitespace_nowrap().overflow_hidden();
    let d = if w > 0.0 { d.w(px(w)).flex_none() } else { d.flex_1().min_w(px(0.)) };
    d.child(s.into())
}

fn row(t: &Theme) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .h(px(theme::ROW_H))
        .px(px(theme::SP2))
        .border_b_1()
        .border_color(t.border.opacity(0.6))
        .text_size(px(theme::TEXT_SM))
}

fn head_row(t: &Theme) -> gpui::Div {
    row(t).text_size(px(theme::TEXT_XS)).text_color(t.text_faint).border_color(t.border)
}

fn note(t: &Theme, s: impl Into<SharedString>) -> gpui::Div {
    div().px(px(theme::PANEL_PX)).py(px(theme::SP2)).text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child(s.into())
}

/// A choice chip (axes, layers).
fn chip(t: &Theme, id: impl Into<gpui::ElementId>, label: &str, on: bool) -> gpui::Stateful<gpui::Div> {
    let hover = t.hover;
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .h(px(26.))
        .px(px(theme::SP2))
        .rounded(px(theme::RADIUS))
        .border_1()
        .border_color(if on { t.accent } else { t.border_strong })
        .when(on, |d| d.bg(t.accent_soft))
        .whitespace_nowrap()
        .text_size(px(theme::TEXT_XS))
        .text_color(if on { t.text } else { t.text_muted })
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(label.to_string())
}

fn short_flight(c: &ModelCurve) -> String {
    c.flight.as_ref().map_or("—".into(), |f| f.reg.clone().unwrap_or_else(|| f.hex.clone()))
}

impl Workspace {
    pub(super) fn toggle_analysis(&mut self, cx: &mut Context<Self>) {
        if self.analysis.is_some() {
            self.close_analysis(cx);
            return;
        }
        self.analysis = Some(Tab::Places);
        log::info!("analysis: {:?}", self.analysis);
        cx.notify();
    }

    /// Close the drawer and take its marks off the simple view: the heat
    /// map, the compared place, the tested place, and the spectrogram if
    /// only the comparison opened it.
    pub(super) fn close_analysis(&mut self, cx: &mut Context<Self>) {
        self.analysis = None;
        self.heat_key = None;
        if let Some(h) = self.heat.take() {
            self.drop_images.push(h.image);
        }
        self.compare = None;
        if let Some(e) = self.explain.as_mut() {
            e.probe_at = None;
        }
        self.spectro_for_analysis = false;
        log::info!("analysis closed");
        cx.notify();
    }

    pub(super) fn act_analysis(&mut self, _: &ToggleAnalysis, _: &mut Window, cx: &mut Context<Self>) {
        if self.explain.is_some() {
            self.toggle_analysis(cx);
        }
    }

    pub(super) fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.analysis = Some(tab);
        if tab == Tab::Heat && self.heat_key.is_none() {
            self.heat_key = Some(HeatKey { scope: Scope::Search, layer: Layer::Combined });
        }
        log::info!("analysis tab: {tab:?}");
        cx.notify();
    }

    /// A new result (or none): forget the analysis of the old one.
    pub(super) fn reset_analysis(&mut self) {
        self.explain = None;
        self.heat_key = None;
        if let Some(h) = self.heat.take() {
            self.drop_images.push(h.image);
        }
        self.compare = None;
        self.score_axes = None;
        self.analysis = None;
        self.spectro_for_analysis = false;
    }

    /// Start what the views on screen need (each piece once, in the background).
    pub(super) fn request_analysis(&mut self, cx: &mut Context<Self>) {
        if self.explain.is_none() {
            return;
        }
        let sel = self.loc_selected;
        let mut wants: Vec<Want> = vec![];
        match self.analysis {
            Some(Tab::Places) => {
                wants.push(Want::Curves(sel));
                wants.extend(self.compare.map(Want::Curves));
                wants.push(Want::Probe);
            }
            Some(Tab::Scores) => wants.push(Want::Events),
            Some(Tab::Silence) => wants.push(Want::Silence(sel)),
            Some(Tab::Heat) | Some(Tab::Timing) | None => {}
        }
        if self.analysis.is_some() && self.show_models {
            wants.push(Want::Curves(sel));
            wants.extend(self.compare.map(Want::Curves));
        }
        if let Some(k) = self.heat_key {
            wants.push(match k.scope {
                Scope::Search => Want::Coarse,
                Scope::Around(c) => Want::Fine(c),
            });
        }
        for w in wants {
            self.want(w, cx);
        }
        self.update_heat();
    }

    fn want(&mut self, w: Want, cx: &mut Context<Self>) {
        let Some(e) = self.explain.as_mut() else { return };
        if e.has(w) {
            return;
        }
        e.running.insert(w);
        let session = e.session.clone();
        let probe_at = e.probe_at;
        let key = Arc::as_ptr(&session) as usize;
        let started = Instant::now();
        cx.spawn(async move |this, cx| {
            let got = cx.background_executor().spawn(async move { explain_data::compute(&session, w, probe_at) }).await;
            this.update(cx, |this, cx| {
                if let Some(e) = this.explain.as_mut()
                    && Arc::as_ptr(&e.session) as usize == key
                {
                    log::info!("analysis {w:?} ready in {:.2} s", started.elapsed().as_secs_f64());
                    e.store(got);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Render the chosen score map once its data is there.
    fn update_heat(&mut self) {
        let Some(key) = self.heat_key else {
            if let Some(h) = self.heat.take() {
                self.drop_images.push(h.image);
            }
            return;
        };
        if self.heat.as_ref().is_some_and(|h| h.key == key) {
            return;
        }
        let Some(e) = &self.explain else { return };
        let h = match key.scope {
            Scope::Search => e.coarse.as_ref().and_then(|m| crate::heat::coarse(m, key)),
            Scope::Around(k) => e.fine.get(&k).cloned().flatten().and_then(|m| crate::heat::fine(&m, key)),
        };
        let ready = match key.scope {
            Scope::Search => e.coarse.is_some(),
            Scope::Around(k) => e.fine.contains_key(&k),
        };
        if h.is_some() || ready {
            if let Some(old) = std::mem::replace(&mut self.heat, h) {
                self.drop_images.push(old.image);
            }
        }
    }

    /// What a heat layer is, in words.
    pub(super) fn heat_label(&self, key: HeatKey) -> String {
        let what = match key.layer {
            Layer::Combined => "All fly-bys together".to_string(),
            Layer::Detection(d) => {
                let app = self.explain.as_ref().map_or(d, |e| e.app_det(d));
                format!("Fly-by {} alone", app + 1)
            }
        };
        let where_ = match key.scope {
            Scope::Search => "whole search".to_string(),
            Scope::Around(k) => match &self.location {
                LocState::Done(r) => place_number(&r.places(), k).map_or("around the place".into(), |n| format!("around place {n}")),
                _ => "around the place".into(),
            },
        };
        format!("{what} · {where_}")
    }

    /// Modelled curves of the selected (and the compared) place for the spectrogram.
    pub(super) fn model_overlays(&self, t: &Theme) -> Vec<ModelOverlay> {
        if self.analysis.is_none() || !self.show_models {
            return vec![];
        }
        let Some(e) = &self.explain else { return vec![] };
        let mut out = vec![];
        let mut add = |k: usize, color: Hsla, tag: &str| {
            let Some(curves) = e.curves.get(&k) else { return };
            for c in curves.iter() {
                let pieces: Vec<Vec<[f64; 2]>> = c.lines.iter().flat_map(|l| l.pieces.iter().cloned()).collect();
                // the label where the lowest line is heard closest
                let label = c.heard_cpa_rec.and_then(|tc| {
                    let low = c.lines.iter().min_by(|a, b| a.harmonic.total_cmp(&b.harmonic))?;
                    let p = low.pieces.iter().flatten().min_by(|a, b| (a[0] - tc).abs().total_cmp(&(b[0] - tc).abs()))?;
                    Some((p[0], p[1], format!("{tag} {} · {:.2}", short_flight(c), c.score)))
                });
                out.push(ModelOverlay { color, pieces, label });
            }
        };
        let sel = self.loc_selected;
        if let Some(b) = self.compare.filter(|&b| b != sel) {
            add(b, t.alt, "B");
        }
        // green, not the blue of the detected lines they are compared with
        add(sel, t.ok, "A");
        out
    }

    /// Pick a place in the analysis (A = the selected one, B = compared with it).
    fn pick(&mut self, k: usize, b: bool, cx: &mut Context<Self>) {
        if b {
            self.compare = if self.compare == Some(k) || k == self.loc_selected { None } else { Some(k) };
        } else {
            self.loc_selected = k;
            if self.compare == Some(k) {
                self.compare = None;
            }
            if self.map_fit == MapFit::Candidate {
                self.map_view = None;
            }
        }
        // the curves are compared on the spectrogram
        if self.show_models && !self.spectro_shown() {
            self.spectro_for_analysis = true;
        }
        cx.notify();
    }

    /// Right click on the map with the analysis open: test that place.
    pub(super) fn probe_at(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let b = self.map_rect.get();
        let v = self.map_view_for(f32::from(b.size.width) as f64, f32::from(b.size.height) as f64);
        let (lat, lon) = v.latlon(&b, pos);
        let sel = self.loc_selected;
        if let Some(e) = self.explain.as_mut() {
            e.probe_at = Some((lat, lon, sel));
            log::info!("probe {lat:.5}, {lon:.5} with the flights of candidate {}", sel + 1);
        }
        self.analysis = Some(Tab::Places);
        cx.notify();
    }

    // ------------------------------------------------------------ the drawer

    pub(super) fn render_analysis(&mut self, t: &Theme, height: f32, with_table_tab: bool, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.analysis.unwrap_or(Tab::Places);
        let hover = t.hover;
        let tab_el = |id: &'static str, label: &str, on: bool| {
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .h_full()
                .px(px(theme::SP3))
                .text_size(px(theme::TEXT_SM))
                .whitespace_nowrap()
                .cursor_pointer()
                .text_color(if on { t.text } else { t.text_muted })
                .when(on, |d| d.border_b_2().border_color(t.accent))
                .hover(move |s| s.bg(hover))
                .child(label.to_string())
        };
        let mut tabs: Vec<AnyElement> = vec![];
        if with_table_tab {
            tabs.push(
                // back to the table: closes the analysis (and its marks)
                tab_el("atab-flybys", "", false)
                    .gap(px(theme::SP1))
                    .child(icon(Icon::ChevronLeft, 10.0, t.text_muted))
                    .child("Fly-bys")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_analysis(cx)))
                    .into_any_element(),
            );
        }
        for (id, label, tb) in [
            ("atab-places", "Places", Tab::Places),
            ("atab-heat", "Heat maps", Tab::Heat),
            ("atab-scores", "Scores", Tab::Scores),
            ("atab-silence", "Silence", Tab::Silence),
            ("atab-timing", "Timing", Tab::Timing),
        ] {
            tabs.push(tab_el(id, label, tb == tab).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_tab(tb, cx))).into_any_element());
        }
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(theme::HEADER_H))
            .pl(px(theme::SP1))
            .pr(px(theme::SP2))
            .border_b_1()
            .border_color(t.border)
            .bg(t.surface)
            .overflow_hidden()
            .child(div().flex_none().px(px(theme::SP2)).text_size(px(theme::TEXT_SM)).font_weight(FontWeight::MEDIUM).text_color(t.text_muted).child("Analysis"))
            .children(tabs)
            .child(div().flex_1())
            .child(
                div()
                    .id("analysis-close")
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(theme::CONTROL_H))
                    .rounded(px(theme::RADIUS))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(icon(Icon::Close, 11.0, t.text_muted))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_analysis(cx))),
            );
        let body: AnyElement = match &self.location {
            LocState::Done(r) if !r.candidates.is_empty() && self.explain.is_some() => {
                let r = r.clone();
                match tab {
                    Tab::Places => self.render_places_tab(t, &r, cx),
                    Tab::Heat => self.render_heat_tab(t, &r, cx),
                    // the scatter fits under the note and the axis chips
                    Tab::Scores => self.render_scores_tab(t, (height - theme::HEADER_H - 120.0).clamp(150.0, 260.0), cx),
                    Tab::Silence => self.render_silence_tab(t, &r),
                    Tab::Timing => self.render_timing_tab(t, &r),
                }
            }
            _ => note(t, "The analysis needs a finished search with at least one place.").into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .h(px(height))
            .bg(t.panel_bg)
            .child(header)
            .child(div().id("analysis-body").flex().flex_col().flex_1().min_h(px(0.)).overflow_y_scroll().child(body))
            .into_any_element()
    }

    fn render_places_tab(&mut self, t: &Theme, r: &LocationResults, cx: &mut Context<Self>) -> AnyElement {
        let places = r.places();
        let sel = self.loc_selected;
        let cmp = self.compare;
        let n_det = self.detections.len();
        let first = places.first().map(|&k| (r.candidates[k].lat, r.candidates[k].lon));
        let radio = |on: bool, color: Hsla| {
            div()
                .size(px(12.))
                .flex_none()
                .rounded(px(6.))
                .border_1()
                .border_color(if on { color } else { t.text_faint })
                .flex()
                .items_center()
                .justify_center()
                .when(on, |d| d.child(div().size(px(6.)).rounded(px(theme::RADIUS_SM)).bg(color)))
        };
        let header = head_row(t)
            .child(cell(28.0, "A"))
            .child(cell(28.0, "B"))
            .child(cell(64.0, "Place"))
            .child(cell(0.0, "Where"))
            .child(cell(56.0, "Chance"))
            .child(cell(66.0, "Fly-bys fit"))
            .child(cell(50.0, "Score"))
            .child(cell(74.0, "Not heard"));
        let rows: Vec<AnyElement> = places
            .iter()
            .take(12)
            .enumerate()
            .map(|(n, &k)| {
                let c = &r.candidates[k];
                let whereabouts = match first {
                    Some((la, lo)) if n > 0 => {
                        let (d, dir) = offset_from(la, lo, c.lat, c.lon);
                        format!("{} {dir} of place 1", fmt_dist_short(d))
                    }
                    _ => format!("{:.4}, {:.4}", c.lat, c.lon),
                };
                let (a_on, b_on) = (k == sel, cmp == Some(k));
                row(t)
                    .when(a_on, |d| d.bg(t.selected))
                    .child(
                        div()
                            .id(("pick-a", k))
                            .w(px(28.))
                            .flex_none()
                            .flex()
                            .justify_center()
                            .cursor_pointer()
                            .child(radio(a_on, t.accent))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.pick(k, false, cx))),
                    )
                    .child(
                        div()
                            .id(("pick-b", k))
                            .w(px(28.))
                            .flex_none()
                            .flex()
                            .justify_center()
                            .cursor_pointer()
                            .child(radio(b_on, t.alt))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.pick(k, true, cx))),
                    )
                    .child(cell(64.0, format!("Place {}", n + 1)).text_color(t.text))
                    .child(cell(0.0, whereabouts).text_size(px(theme::TEXT_XS)).text_color(t.text_muted))
                    .child(cell(56.0, c.probability.map(fmt_chance).unwrap_or_default()).font_family(MONO).text_size(px(theme::TEXT_XS)))
                    .child(cell(66.0, format!("{} of {n_det}", c.matched)).font_family(MONO).text_size(px(theme::TEXT_XS)))
                    .child(cell(50.0, format!("{:.2}", c.raw_score)).font_family(MONO).text_size(px(theme::TEXT_XS)))
                    .child(
                        cell(74.0, if c.violations == 0 { "—".to_string() } else { format!("{} aircraft", c.violations) })
                            .text_size(px(theme::TEXT_XS))
                            .text_color(if c.violations > 0 { t.warn } else { t.text_faint }),
                    )
                    .into_any_element()
            })
            .collect();
        // the two side by side, fly-by by fly-by
        let e = self.explain.as_ref();
        let curves_a = e.and_then(|e| e.curves.get(&sel).cloned());
        let curves_b = cmp.and_then(|b| e.and_then(|e| e.curves.get(&b).cloned()));
        let app_det = |k: usize| e.map_or(k, |e| e.app_det(k));
        let mut dets: Vec<usize> = curves_a.iter().chain(curves_b.iter()).flat_map(|c| c.iter().map(|m| m.detection)).collect();
        dets.sort_unstable();
        dets.dedup();
        let cmp_rows: Vec<AnyElement> = dets
            .iter()
            .map(|&k| {
                let find = |cs: &Option<Arc<Vec<ModelCurve>>>| cs.as_ref().and_then(|c| c.iter().find(|m| m.detection == k).cloned());
                let (a, b) = (find(&curves_a), find(&curves_b));
                let i = app_det(k);
                let tc = self.detections.get(i).map(|d| crate::draw::fmt_time(d.flyby.tc, 1.0)).unwrap_or_default();
                let fmt = |m: &Option<ModelCurve>| m.as_ref().map_or("—".to_string(), |m| format!("{}  {:.2}", short_flight(m), m.score));
                row(t)
                    .child(cell(90.0, format!("Fly-by {}", i + 1)).text_color(t.text))
                    .child(cell(70.0, tc).font_family(MONO).text_size(px(theme::TEXT_XS)).text_color(t.text_muted))
                    .child(cell(0.0, fmt(&a)).font_family(MONO).text_size(px(theme::TEXT_XS)).text_color(t.ok))
                    .when(cmp.is_some(), |d| d.child(cell(0.0, fmt(&b)).font_family(MONO).text_size(px(theme::TEXT_XS)).text_color(t.alt)))
                    .into_any_element()
            })
            .collect();
        let loading = e.is_some_and(|e| e.running.contains(&Want::Curves(sel)) || cmp.is_some_and(|b| e.running.contains(&Want::Curves(b))));
        let show_models = self.show_models;
        let spectro = self.spectro_shown();
        let probe = e.and_then(|e| e.probe.clone().filter(|p| Some((p.lat, p.lon, p.from)) == e.probe_at));
        let probing = e.is_some_and(|e| e.probe_at.is_some() && e.running.contains(&Want::Probe));
        let a_n = place_number(&places, sel).map_or("the selected place".into(), |n| format!("place {n}"));
        let probe_el: Option<AnyElement> = if probing {
            Some(note(t, "Testing the place you picked…").into_any_element())
        } else {
            probe.map(|p| {
                let base = r.candidates.get(p.from).map_or(0.0, |c| c.raw_score);
                let (text, col) = match &p.error {
                    Some(err) => (format!("Your test place {:.5}, {:.5}: {err}", p.lat, p.lon), t.danger),
                    None => {
                        let fit = p.curves.iter().filter(|c| c.score >= 0.45).count();
                        let silent = p.silence.as_ref().map_or(0, |s| s.violations);
                        let quiet = match silent {
                            0 => "no aircraft is missing from the recording".to_string(),
                            1 => "1 aircraft that would have been heard there is missing from the recording".to_string(),
                            n => format!("{n} aircraft that would have been heard there are missing from the recording"),
                        };
                        let good = p.score >= base - 0.5 && silent == 0;
                        (
                            format!(
                                "Your test place {:.5}, {:.5}: with the aircraft of {a_n}, {fit} fly-by{} fit, score {:.2} ({a_n}: {:.2}); {quiet}.",
                                p.lat,
                                p.lon,
                                if fit == 1 { "" } else { "s" },
                                p.score,
                                base
                            ),
                            if good { t.ok } else { t.warn },
                        )
                    }
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(theme::SP2))
                    .px(px(theme::PANEL_PX))
                    .py(px(theme::SP2))
                    .child(div().flex_1().min_w(px(0.)).text_size(px(theme::TEXT_XS)).text_color(col).child(text))
                    .child(toggle(t, "probe-clear", "Clear", false).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(e) = this.explain.as_mut() {
                            e.probe_at = None;
                            e.probe = None;
                        }
                        cx.notify();
                    })))
                    .into_any_element()
            })
        };
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(theme::SP2))
                    .px(px(theme::PANEL_PX))
                    .py(px(theme::SP2))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_size(px(theme::TEXT_XS))
                            .text_color(t.text_muted)
                            .child("A is the place on the map; pick B to compare. The curves their aircraft would make are drawn dashed on the spectrogram over the recorded lines (A green, B pink). Right-click the map to test any other place."),
                    )
                    .child(switch(t, "models-btn", "Curves on the spectrogram", show_models && spectro).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if show_models && this.spectro_shown() {
                            this.show_models = false;
                        } else {
                            this.show_models = true;
                            if !this.spectro_shown() {
                                this.spectro_for_analysis = true;
                            }
                        }
                        cx.notify();
                    }))),
            )
            .child(header)
            .children(rows)
            .children(probe_el)
            .when(!cmp_rows.is_empty() || loading, |d| {
                d.child(section_header(t, "Fly-by by fly-by", Some(label_xs(t, "aircraft and curve score (1 = as good as the detected line)").into_any_element())))
                    .when(loading, |d| d.child(note(t, "Modelling the curves…")))
                    .children(cmp_rows)
            })
            .into_any_element()
    }

    fn render_heat_tab(&mut self, t: &Theme, r: &LocationResults, cx: &mut Context<Self>) -> AnyElement {
        let key = self.heat_key.unwrap_or(HeatKey { scope: Scope::Search, layer: Layer::Combined });
        let sel = self.loc_selected;
        let places = r.places();
        let e = self.explain.as_ref();
        let n_session = e.map_or(0, |e| e.session.dets.len());
        // the fly-bys a layer can show: in the whole search every one; around a place, the ones its aircraft explain
        let layers: Vec<usize> = match key.scope {
            Scope::Search => (0..n_session).collect(),
            Scope::Around(k) => e
                .and_then(|e| e.fine.get(&k).cloned().flatten())
                .map(|m| m.per_detection.iter().filter_map(|l| l.detection).collect())
                .unwrap_or_default(),
        };
        let busy = e.is_some_and(|e| match key.scope {
            Scope::Search => e.running.contains(&Want::Coarse),
            Scope::Around(k) => e.running.contains(&Want::Fine(k)),
        });
        let unavailable = !busy
            && e.is_some_and(|e| match key.scope {
                Scope::Search => e.coarse.is_some(),
                Scope::Around(k) => e.fine.contains_key(&k),
            })
            && self.heat.is_none();
        let a_n = place_number(&places, sel).map_or("the place".to_string(), |n| format!("place {n}"));
        let set = |this: &mut Self, k: HeatKey, cx: &mut Context<Self>| {
            this.heat_key = Some(k);
            // the map shows it: frame the result for the fine map, the search for the coarse one
            this.map_view = None;
            this.map_fit = match k.scope {
                Scope::Search => MapFit::Area,
                Scope::Around(_) => MapFit::Candidate,
            };
            log::info!("heat map: {k:?}");
            cx.notify();
        };
        let scope_row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(theme::SP1))
            .px(px(theme::PANEL_PX))
            .pt(px(theme::SP2))
            .child(div().w(px(64.)).flex_none().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child("Where"))
            .child(chip(t, "heat-scope-search", "Whole search area", key.scope == Scope::Search).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                set(this, HeatKey { scope: Scope::Search, layer: Layer::Combined }, cx)
            })))
            .child(chip(t, "heat-scope-place", &format!("Around {a_n} (100 m grid)"), key.scope == Scope::Around(sel)).on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| set(this, HeatKey { scope: Scope::Around(sel), layer: Layer::Combined }, cx),
            )));
        let mut layer_chips: Vec<AnyElement> = vec![chip(t, "heat-layer-all", "All fly-bys together", key.layer == Layer::Combined)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| set(this, HeatKey { layer: Layer::Combined, ..key }, cx)))
            .into_any_element()];
        for d in layers {
            let app = e.map_or(d, |e| e.app_det(d));
            let aircraft = r.candidates.get(sel).and_then(|c| c.tracks.iter().find(|tr| tr.detection == app)).map(|tr| format!(" · {}", tr.callsign)).unwrap_or_default();
            layer_chips.push(
                chip(t, ("heat-layer", d), &format!("Fly-by {}{aircraft}", app + 1), key.layer == Layer::Detection(d))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| set(this, HeatKey { layer: Layer::Detection(d), ..key }, cx)))
                    .into_any_element(),
            );
        }
        let layer_row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(theme::SP1))
            .px(px(theme::PANEL_PX))
            .pt(px(theme::SP2))
            .child(div().w(px(64.)).flex_none().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child("Score of"))
            .children(layer_chips);
        let shown = self.heat_key.is_some();
        let status = if busy {
            "Computing the score map…".to_string()
        } else if unavailable {
            "No score map here: no aircraft explains this fly-by in that area.".to_string()
        } else if let Some(h) = &self.heat {
            format!("{}: best {:.2} at {:.5}, {:.5}.", self.heat_label(h.key), h.max, h.best.0, h.best.1)
        } else {
            String::new()
        };
        div()
            .flex()
            .flex_col()
            .child(note(t, "How well each place explains the fly-bys, drawn on the map (brighter = fits better). One fly-by alone fits a band of places; all of them together, with one start time, fit one point."))
            .child(scope_row)
            .child(layer_row)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(theme::SP2))
                    .px(px(theme::PANEL_PX))
                    .py(px(theme::SP2))
                    .child(div().flex_1().min_w(px(0.)).text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child(status))
                    .child(toggle(t, "heat-hide", if shown { "Hide from the map" } else { "Show on the map" }, false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.heat_key = if shown { None } else { Some(key) };
                        cx.notify();
                    }))),
            )
            .into_any_element()
    }

    fn render_scores_tab(&mut self, t: &Theme, plot_h: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(e) = self.explain.as_ref() else { return div().into_any_element() };
        let Some(ev) = e.events.clone() else {
            return note(t, "Scoring every place the search refined…").into_any_element();
        };
        let n = ev.detections.len();
        // fly-bys with at least one match among the points, most matched first
        let mut counts: Vec<(usize, usize)> = (0..n)
            .map(|d| (d, ev.points.iter().filter(|p| p.per_detection.get(d).and_then(|x| x.as_ref()).is_some_and(|s| s.score >= ev.min_score)).count()))
            .collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let default = match counts.as_slice() {
            [a, b, ..] if b.1 > 0 => (a.0, b.0),
            [a, ..] => (a.0, usize::MAX),
            [] => (0, usize::MAX),
        };
        let (ax, ay) = self.score_axes.unwrap_or(default);
        let app = |d: usize| e.app_det(d);
        let score = |p: &EventPoint, d: usize| -> f64 {
            if d == usize::MAX {
                p.total
            } else {
                p.per_detection.get(d).and_then(|x| x.as_ref()).map_or(0.0, |s| s.score)
            }
        };
        let flight = |p: &EventPoint, d: usize| -> String {
            p.per_detection
                .get(d)
                .and_then(|x| x.as_ref())
                .filter(|s| s.score >= ev.min_score)
                .map_or("none".into(), |s| s.flight.reg.clone().unwrap_or_else(|| s.flight.hex.clone()))
        };
        // groups: the flights given to the two fly-bys
        let key = |p: &EventPoint| if ay == usize::MAX { flight(p, ax) } else { format!("{} + {}", flight(p, ax), flight(p, ay)) };
        let mut groups: Vec<(String, f64, usize, Option<usize>)> = vec![];
        for p in &ev.points {
            let k = key(p);
            match groups.iter_mut().find(|g| g.0 == k) {
                Some(g) => {
                    g.2 += 1;
                    if p.total > g.1 {
                        g.1 = p.total;
                    }
                    g.3 = g.3.or(p.candidate_rank);
                }
                None => groups.push((k, p.total, 1, p.candidate_rank)),
            }
        }
        groups.sort_by(|a, b| b.1.total_cmp(&a.1));
        let points: Vec<(f64, f64, usize, bool)> = ev
            .points
            .iter()
            .map(|p| {
                let g = groups.iter().position(|g| g.0 == key(p)).unwrap_or(0);
                let best = (p.total - groups[g].1).abs() < 1e-9;
                (score(p, ax), score(p, ay), g, best)
            })
            .collect();
        let x_max = points.iter().map(|p| p.0).fold(1.0f64, f64::max) * 1.05;
        let y_max = points.iter().map(|p| p.1).fold(1.0f64, f64::max) * 1.05;
        let y_min = if ay == usize::MAX { points.iter().map(|p| p.1).fold(0.0f64, f64::min) } else { 0.0 };
        let x_name = format!("Fly-by {}", app(ax) + 1);
        let y_name = if ay == usize::MAX { "Total score".to_string() } else { format!("Fly-by {}", app(ay) + 1) };
        let theme_c = *t;
        let min_score = ev.min_score;
        let scatter = canvas(|_, _, _| {}, move |b, _, window, cx| {
            let t = theme_c;
            let plot = Bounds::new(point(b.left() + px(40.), b.top() + px(8.)), size(b.size.width - px(52.), b.size.height - px(34.)));
            crate::draw::rect(window, plot, t.field_bg);
            crate::draw::frame(window, plot, t.border);
            let xp = |x: f64| plot.left() + plot.size.width * (x / x_max) as f32;
            let yp = |y: f64| plot.bottom() - plot.size.height * ((y - y_min) / (y_max - y_min).max(1e-9)) as f32;
            // the match threshold on each axis
            crate::draw::vline(window, xp(min_score), plot.top(), plot.bottom(), t.text_faint.opacity(0.5));
            if ay != usize::MAX {
                crate::draw::hline(window, plot.left(), plot.right(), yp(min_score), t.text_faint.opacity(0.5));
            }
            for k in 0..=4 {
                let v = x_max * k as f64 / 4.0;
                crate::draw::text(window, cx, format!("{v:.1}"), point(xp(v), plot.bottom() + px(3.)), 9.5, t.text_faint, true, crate::draw::Align::Center);
                let w = y_min + (y_max - y_min) * k as f64 / 4.0;
                crate::draw::text(window, cx, format!("{w:.1}"), point(plot.left() - px(4.), yp(w) - px(6.)), 9.5, t.text_faint, true, crate::draw::Align::Right);
            }
            crate::draw::text(window, cx, x_name.clone(), point(plot.left() + plot.size.width / 2.0, plot.bottom() + px(15.)), 10.0, t.text_muted, false, crate::draw::Align::Center);
            crate::draw::text(window, cx, y_name.clone(), point(plot.left() + px(4.), plot.top() + px(2.)), 10.0, t.text_muted, false, crate::draw::Align::Left);
            for &(x, y, g, best) in points.iter().rev() {
                let c = pair_color(g);
                let p = point(xp(x), yp(y));
                if best {
                    crate::draw::circle(window, p, 5.5, Some(c), Some((1.5, t.text)));
                } else {
                    crate::draw::circle(window, p, 2.6, Some(c.opacity(0.75)), None);
                }
            }
        })
        .flex_1()
        .min_w(px(220.))
        .h(px(plot_h));
        let axis_chips = |y: bool, cx: &mut Context<Self>| -> Vec<AnyElement> {
            let mut v: Vec<AnyElement> = counts
                .iter()
                .filter(|c| c.1 > 0 || n <= 2)
                .map(|&(d, _)| {
                    let on = if y { ay == d } else { ax == d };
                    chip(t, (if y { "ay" } else { "ax" }, d), &format!("Fly-by {}", app(d) + 1), on)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let (x0, y0) = this.score_axes.unwrap_or(default);
                            this.score_axes = Some(if y { (x0, d) } else { (d, y0) });
                            cx.notify();
                        }))
                        .into_any_element()
                })
                .collect();
            if y {
                v.push(
                    chip(t, "ay-total", "Total score", ay == usize::MAX)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let (x0, _) = this.score_axes.unwrap_or(default);
                            this.score_axes = Some((x0, usize::MAX));
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
            v
        };
        let xs = axis_chips(false, cx);
        let ys = axis_chips(true, cx);
        let list: Vec<AnyElement> = groups
            .iter()
            .take(8)
            .enumerate()
            .map(|(i, g)| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(theme::SP2))
                    .h(px(26.))
                    .text_size(px(theme::TEXT_XS))
                    .child(div().size(px(8.)).flex_none().rounded(px(4.)).bg(pair_color(i)))
                    .child(div().flex_1().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_color(t.text).child(g.0.clone()))
                    .child(mono(format!("{:.2}", g.1)).flex_none().text_color(t.text_muted))
                    .child(mono(g.3.map(|r| format!("#{r}")).unwrap_or_default()).w(px(28.)).flex_none().text_color(t.text_faint))
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .child(note(t, "Every place the search refined, scored fly-by by fly-by (1 = as good as the detected line; the faint lines are the match threshold). Colours group the places by the aircraft they give the two fly-bys; the ringed point is the best of each group."))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(theme::SP1))
                    .px(px(theme::PANEL_PX))
                    .child(div().w(px(56.)).flex_none().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child("Across"))
                    .children(xs),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(theme::SP1))
                    .px(px(theme::PANEL_PX))
                    .pt(px(theme::SP1))
                    .child(div().w(px(56.)).flex_none().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child("Up"))
                    .children(ys),
            )
            .child(
                div()
                    .flex()
                    .gap(px(theme::SP3))
                    .px(px(theme::PANEL_PX))
                    .py(px(theme::SP2))
                    .child(scatter)
                    .child(
                        div()
                            .w(px(250.))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .child(div().h(px(26.)).text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child("Aircraft given to the fly-bys · best score · rank"))
                            .children(list),
                    ),
            )
            .into_any_element()
    }

    fn render_silence_tab(&mut self, t: &Theme, r: &LocationResults) -> AnyElement {
        let sel = self.loc_selected;
        let Some(e) = self.explain.as_ref() else { return div().into_any_element() };
        let a_n = place_number(&r.places(), sel).map_or("this place".to_string(), |n| format!("place {n}"));
        let s = match e.silence.get(&sel) {
            Some(Some(s)) => s.clone(),
            Some(None) => return note(t, "No silence check for this place.").into_any_element(),
            None => return note(t, "Checking which aircraft passed close…").into_any_element(),
        };
        let summary = if s.violations == 0 {
            format!("Every aircraft that passed close to {a_n} while recording is heard in the recording (or passed too far away to hear for sure).")
        } else {
            format!(
                "{} aircraft passed within {} of {a_n} while recording but are not heard: each counts against this place.",
                s.violations,
                fmt_dist_short(s.quiet_d_m)
            )
        };
        let rows: Vec<AnyElement> = s
            .passes
            .iter()
            .map(|p| {
                let (text, col) = match (p.status.as_str(), p.detection) {
                    ("matched", Some(d)) => (format!("heard: fly-by {}", e.app_det(d) + 1), t.ok),
                    ("near_detection", Some(d)) => (format!("near fly-by {}", e.app_det(d) + 1), t.text_muted),
                    ("silent", _) if p.counts_against => ("not heard: counts against".to_string(), t.danger),
                    ("silent", _) => ("not heard (far enough)".to_string(), t.text_faint),
                    ("edge", _) => ("at the start or end of a part".to_string(), t.text_faint),
                    (s, _) => (s.to_string(), t.text_faint),
                };
                row(t)
                    .child(cell(80.0, crate::draw::fmt_time(p.heard_rec, 1.0)).font_family(MONO).text_size(px(theme::TEXT_XS)))
                    .child(cell(0.0, p.flight.label()).text_color(t.text))
                    .child(cell(80.0, fmt_dist_short(p.slant_m)).font_family(MONO).text_size(px(theme::TEXT_XS)).text_color(t.text_muted))
                    .child(cell(190.0, text).text_size(px(theme::TEXT_XS)).text_color(col))
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .child(note(t, summary))
            .child(head_row(t).child(cell(80.0, "Heard at")).child(cell(0.0, "Aircraft")).child(cell(80.0, "Closest")).child(cell(190.0, "In the recording")))
            .children(rows)
            .when(s.passes.is_empty(), |d| d.child(note(t, format!("No aircraft of the search passed within {} while recording.", fmt_dist_short(s.radius_m)))))
            .into_any_element()
    }

    fn render_timing_tab(&mut self, t: &Theme, r: &LocationResults) -> AnyElement {
        let sel = self.loc_selected;
        let Some(c) = r.candidates.get(sel) else { return div().into_any_element() };
        let mut tracks: Vec<&crate::location::MatchedTrack> = c.tracks.iter().collect();
        tracks.sort_by(|a, b| a.heard_rec.total_cmp(&b.heard_rec));
        let a_n = place_number(&r.places(), sel).map_or("this place".to_string(), |n| format!("place {n}"));
        let dets = &self.detections;
        let cuts = r.cuts.clone();
        let part = |t: f64| cuts.iter().filter(|&&c| c < t).count();
        let mut prev: Option<(f64, f64)> = None;
        let rows: Vec<AnyElement> = tracks
            .iter()
            .map(|tr| {
                let tc = dets.get(tr.detection).map_or(tr.heard_rec - tr.dt_s, |d| d.flyby.tc);
                let gap = prev.map(|(pt, ph)| (tc - pt, tr.heard_rec - ph));
                prev = Some((tc, tr.heard_rec));
                let off = tr.heard_rec - tc;
                row(t)
                    .child(cell(70.0, format!("Fly-by {}", tr.detection + 1)).text_color(t.text))
                    .when(!cuts.is_empty(), |d| d.child(cell(44.0, format!("{}", part(tc) + 1)).font_family(MONO).text_size(px(theme::TEXT_XS))))
                    .child(cell(0.0, tr.callsign.clone()).text_color(t.text))
                    .child(cell(80.0, crate::draw::fmt_time(tc, 1.0)).font_family(MONO).text_size(px(theme::TEXT_XS)))
                    .child(cell(80.0, crate::draw::fmt_time(tr.heard_rec, 1.0)).font_family(MONO).text_size(px(theme::TEXT_XS)))
                    .child(cell(60.0, format!("{off:+.1} s")).font_family(MONO).text_size(px(theme::TEXT_XS)).text_color(if off.abs() > 5.0 { t.warn } else { t.text_muted }))
                    .child(
                        cell(130.0, gap.map_or("—".to_string(), |(g, m)| format!("{} / {}", crate::draw::fmt_time(g, 1.0), crate::draw::fmt_time(m, 1.0))))
                            .font_family(MONO)
                            .text_size(px(theme::TEXT_XS))
                            .text_color(t.text_muted),
                    )
                    .into_any_element()
            })
            .collect();
        // the two rows of times side by side
        let dur = self.duration().max(1.0);
        let marks: Vec<(f64, f64)> = tracks.iter().map(|tr| (dets.get(tr.detection).map_or(tr.heard_rec, |d| d.flyby.tc), tr.heard_rec)).collect();
        let theme_c = *t;
        let cuts2 = r.cuts.clone();
        let chart = canvas(|_, _, _| {}, move |b, _, window, cx| {
            let t = theme_c;
            let (x0, w) = (b.left() + px(110.), b.size.width - px(124.));
            let x = |s: f64| x0 + w * (s / dur) as f32;
            let (y1, y2) = (b.top() + px(12.), b.top() + px(40.));
            crate::draw::text(window, cx, "In the recording", point(b.left() + px(8.), y1 - px(7.)), 10.0, t.text_muted, false, crate::draw::Align::Left);
            crate::draw::text(window, cx, "Modelled", point(b.left() + px(8.), y2 - px(7.)), 10.0, t.text_muted, false, crate::draw::Align::Left);
            crate::draw::hline(window, x0, x0 + w, y1, t.border_strong);
            crate::draw::hline(window, x0, x0 + w, y2, t.border_strong);
            for &c in &cuts2 {
                crate::draw::rect_xywh(window, x(c) - px(1.), y1 - px(8.), px(2.), y2 - y1 + px(16.), t.danger);
            }
            for &(a, m) in &marks {
                crate::draw::polyline(window, &[point(x(a), y1), point(x(m), y2)], 1.0, t.text_faint);
                crate::draw::rect_xywh(window, x(a) - px(1.), y1 - px(6.), px(2.), px(12.), t.text);
                crate::draw::rect_xywh(window, x(m) - px(1.), y2 - px(6.), px(2.), px(12.), t.accent);
            }
        })
        .w_full()
        .h(px(52.));
        div()
            .flex()
            .flex_col()
            .child(note(
                t,
                format!(
                    "{} Every aircraft of {a_n} must be heard when its fly-by is. \"Off by\" is the modelled time minus the detected one; the gaps between fly-bys must agree too.",
                    match r.cuts.len() {
                        0 => "The search fits one start time to the whole recording.".to_string(),
                        n => format!("The recording has {n} cut{}, so each of its {} parts gets its own start time.", if n == 1 { "" } else { "s" }, n + 1),
                    }
                ),
            ))
            .child(div().px(px(theme::PANEL_PX)).child(chart))
            .child(
                head_row(t)
                    .child(cell(70.0, "Fly-by"))
                    .when(!cuts.is_empty(), |d| d.child(cell(44.0, "Part")))
                    .child(cell(0.0, "Aircraft"))
                    .child(cell(80.0, "Detected at"))
                    .child(cell(80.0, "Modelled at"))
                    .child(cell(60.0, "Off by"))
                    .child(cell(130.0, "Gap: detected / model")),
            )
            .children(rows)
            .when(tracks.is_empty(), |d| d.child(note(t, "No aircraft matched at this place.")))
            .into_any_element()
    }
}
