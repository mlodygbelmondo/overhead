//! The timeline bar: play the recording from the cursor (optionally with
//! the aircraft's band boosted, as in the reference video), and the cuts
//! (where material was removed: each part gets its own start time) shown,
//! added, moved and removed on the timeline, with the audio's suggestions.

use super::*;
use crate::timeline::{self, Hit, Mark, TimelineScene};
use overhead_core::cuts::{CutConfig, CutSuggestion};
use overhead_core::listen::ListenSettings;

/// Closest two cuts may be (s), and to either end of the recording.
const CUT_MIN_GAP: f64 = 2.0;

impl Workspace {
    // ------------------------------------------------------------ playing

    pub(super) fn act_play(&mut self, _: &TogglePlay, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_play(cx);
    }

    fn listen_settings(&self) -> ListenSettings {
        let mut s = if self.boost { ListenSettings::boost(self.boost_band.0, self.boost_band.1) } else { ListenSettings::default() };
        s.band_gain_db = self.boost_db;
        s.gain_db = self.volume_db;
        s
    }

    fn apply_listen(&self) {
        if let Some(p) = &self.player {
            p.set_settings(self.listen_settings());
        }
    }

    pub(super) fn is_playing(&self) -> bool {
        self.player.as_ref().is_some_and(|p| p.is_playing())
    }

    pub(super) fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if self.file.as_ref().and_then(|f| f.audio.as_ref()).is_none() {
            return;
        }
        if let Some(p) = &self.player {
            if p.is_playing() {
                p.pause();
                log::info!("paused at {:.1} s", p.position());
            } else {
                let from = self.cursor_t.unwrap_or(0.0);
                let from = if from >= self.duration() - 0.2 { 0.0 } else { from };
                p.play_from(from, None);
                log::info!("playing from {from:.1} s (boost {})", self.boost);
                self.start_play_ticker(cx);
            }
            cx.notify();
            return;
        }
        if self.player_state == PlayerState::Opening {
            return;
        }
        // first play: decode for listening (a better rate than the analysis
        // audio, when that fits in memory) and open the sound device
        let Some(audio) = self.file.as_ref().and_then(|f| f.audio.clone()) else { return };
        self.player_state = PlayerState::Opening;
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let opened = cx
                .background_executor()
                .spawn(async move {
                    let a = crate::audio::load_for_listening(&audio).unwrap_or(audio);
                    let rate = a.sample_rate;
                    crate::audio::Player::new(a).map(|p| (p, rate))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match opened {
                    Ok((p, rate)) => {
                        log::info!("player ready: {} at {} Hz (audio {rate:.0} Hz)", p.device, p.out_rate);
                        this.player = Some(p);
                        this.player_state = PlayerState::Ready;
                        this.apply_listen();
                        this.toggle_play(cx);
                    }
                    Err(e) => {
                        log::warn!("no sound: {e:#}");
                        this.player_state = PlayerState::Failed(format!("{e:#}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// While playing: the cursor follows the sound (and the spectrogram pages along).
    fn start_play_ticker(&mut self, cx: &mut Context<Self>) {
        self.play_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(33)).await;
                let keep = this
                    .update(cx, |this, cx| {
                        let Some(p) = &this.player else { return false };
                        let pos = p.position();
                        let playing = p.is_playing();
                        this.cursor_t = Some(pos);
                        // page the spectrogram when the sound leaves its window
                        let v = this.view;
                        if this.spectro_shown() && (pos > v.t1 || pos < v.t0) && v.t1 - v.t0 < this.duration() - 0.5 {
                            let span = v.t1 - v.t0;
                            this.view.t0 = (pos - 0.1 * span).max(0.0);
                            this.view.t1 = this.view.t0 + span;
                            this.clamp_view();
                        }
                        cx.notify();
                        if !playing {
                            log::info!("stopped at {pos:.1} s");
                        }
                        playing
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    /// Move the playing position (and the cursor everything shares).
    fn seek_to(&mut self, t: f64) {
        let t = t.clamp(0.0, self.duration());
        self.cursor_t = Some(t);
        if let Some(p) = &self.player {
            p.seek(t);
        }
    }

    /// The boost band: the selected fly-by's lines, else a band for propellers.
    fn default_band(&self) -> (f64, f64) {
        match self.selected.and_then(|i| self.curves.get(i)) {
            Some(c) => {
                // the lines heard: `f0` itself may not be one of them
                let rmin = c.ratios.iter().copied().fold(f64::INFINITY, f64::min);
                let rmin = if rmin.is_finite() { rmin } else { 1.0 };
                let rmax = c.ratios.iter().copied().fold(rmin, f64::max).min(rmin * 4.0);
                let lo = c.flyby.f_recede(DEFAULT_C) * rmin * 0.9;
                let hi = c.flyby.f_approach(DEFAULT_C) * rmax * 1.1;
                ((lo / 5.0).floor() * 5.0, (hi / 5.0).ceil() * 5.0)
            }
            None => (80.0, 500.0),
        }
    }

    fn set_band(&mut self, band: (f64, f64), cx: &mut Context<Self>) {
        self.boost_band = (band.0.min(band.1).max(10.0), band.0.max(band.1).max(20.0));
        let (lo, hi) = self.boost_band;
        self.boost_lo.update(cx, |i, cx| i.set_text(&format!("{lo:.0}"), cx));
        self.boost_hi.update(cx, |i, cx| i.set_text(&format!("{hi:.0}"), cx));
        self.apply_listen();
    }

    /// The band fields changed: use them when both are numbers in order.
    pub(super) fn band_typed(&mut self, cx: &mut Context<Self>) {
        // an empty field keeps its end of the band (the placeholder shows it)
        let read = |f: &Entity<TextInput>, cur: f64, cx: &Context<Self>| {
            let s = f.read(cx).text().trim().to_string();
            if s.is_empty() { Ok(cur) } else { s.parse::<f64>() }
        };
        let lo = read(&self.boost_lo, self.boost_band.0, cx);
        let hi = read(&self.boost_hi, self.boost_band.1, cx);
        if let (Ok(lo), Ok(hi)) = (lo, hi)
            && lo > 0.0
            && hi > lo
        {
            self.boost_band = (lo, hi);
            self.apply_listen();
        }
        cx.notify();
    }

    // ------------------------------------------------------------ cuts

    /// Look for places the audio may have been cut (background, after detection).
    pub(super) fn start_cut_suggestions(&mut self, cx: &mut Context<Self>) {
        let Some(audio) = self.file.as_ref().and_then(|f| f.audio.clone()) else { return };
        let generation = self.generation;
        self.cuts_task = Some(cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let s = cx.background_executor().spawn(async move { overhead_core::cuts::suggest(&audio, &CutConfig::default()) }).await;
            this.update(cx, |this, cx| {
                if this.generation == generation {
                    log::info!("cut suggestions: {} in {:.1} s", s.len(), started.elapsed().as_secs_f64());
                    this.cut_suggestions = s;
                    this.cuts_task = None;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Suggestions not taken yet, in time order.
    fn open_suggestions(&self) -> Vec<&CutSuggestion> {
        let mut v: Vec<&CutSuggestion> = self.cut_suggestions.iter().filter(|s| !self.cuts.iter().any(|&c| (c - s.at).abs() < 1.0)).collect();
        v.sort_by(|a, b| a.at.total_cmp(&b.at));
        v
    }

    pub(super) fn add_cut(&mut self, t: f64) -> bool {
        let dur = self.duration();
        if t < CUT_MIN_GAP || t > dur - CUT_MIN_GAP || self.cuts.iter().any(|&c| (c - t).abs() < CUT_MIN_GAP) {
            return false;
        }
        self.push_undo();
        self.cuts.push(t);
        self.cuts.sort_by(f64::total_cmp);
        log::info!("cut added at {t:.1} s ({} cuts)", self.cuts.len());
        true
    }

    fn remove_cut(&mut self, i: usize) {
        if i < self.cuts.len() {
            self.push_undo();
            let c = self.cuts.remove(i);
            log::info!("cut at {c:.1} s removed ({} cuts)", self.cuts.len());
        }
    }

    fn move_cut(&mut self, i: usize, t: f64) {
        let dur = self.duration();
        let lo = if i > 0 { self.cuts[i - 1] + CUT_MIN_GAP } else { CUT_MIN_GAP };
        let hi = if i + 1 < self.cuts.len() { self.cuts[i + 1] - CUT_MIN_GAP } else { dur - CUT_MIN_GAP };
        if hi > lo {
            self.cuts[i] = t.clamp(lo, hi);
        }
    }

    fn timeline_scene(&self, t: &Theme) -> TimelineScene {
        let matched: Vec<bool> = match &self.location {
            LocState::Done(r) if !self.dets_changed => {
                let c = r.candidates.get(self.loc_selected);
                (0..self.detections.len()).map(|i| c.is_some_and(|c| c.tracks.iter().any(|tr| tr.detection == i))).collect()
            }
            _ => vec![false; self.detections.len()],
        };
        TimelineScene {
            duration: self.duration(),
            marks: self
                .detections
                .iter()
                .enumerate()
                .map(|(i, d)| Mark { t: d.flyby.tc, matched: matched.get(i).copied().unwrap_or(false), selected: self.selected == Some(i) })
                .collect(),
            cuts: self.cuts.clone(),
            suggested: if self.bar_panel == Some(BarPanel::Cuts) { self.open_suggestions().iter().map(|s| s.at).collect() } else { vec![] },
            cursor: self.cursor_t,
            hover: self.tl_hover,
            dragging: match self.tl_drag {
                Some(TlDrag::Cut(i)) => Some(i),
                _ => None,
            },
            window: self.spectro_shown().then_some((self.view.t0, self.view.t1)),
            theme: *t,
        }
    }

    fn on_tl_down(&mut self, ev: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let b = self.timeline_rect.get();
        let dur = self.duration();
        if dur <= 0.0 {
            return;
        }
        let t = theme::theme(cx);
        let scene = self.timeline_scene(&t);
        let at = timeline::t_at(b, dur, ev.position.x);
        match timeline::hit(b, &scene, ev.position) {
            Hit::Cut(i) => {
                // one undo step for the whole drag, kept only if the cut moved
                self.cut_drag_from = Some(self.cuts.clone());
                self.tl_drag = Some(TlDrag::Cut(i));
            }
            Hit::Suggested(i) => {
                if let Some(s) = scene.suggested.get(i).copied() {
                    self.add_cut(s);
                }
            }
            Hit::Track if ev.click_count >= 2 => {
                // a double click adds a cut there
                self.add_cut(at);
            }
            Hit::Track => {
                self.tl_drag = Some(TlDrag::Seek);
                self.seek_to(at);
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn on_tl_right(&mut self, ev: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let b = self.timeline_rect.get();
        let t = theme::theme(cx);
        let scene = self.timeline_scene(&t);
        if let Hit::Cut(i) = timeline::hit(b, &scene, ev.position) {
            self.remove_cut(i);
            cx.notify();
        }
    }

    fn on_tl_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let b = self.timeline_rect.get();
        let dur = self.duration();
        if dur <= 0.0 {
            return;
        }
        let at = timeline::t_at(b, dur, ev.position.x);
        let inside = b.contains(&ev.position);
        self.tl_hover = inside.then_some(at);
        match self.tl_drag {
            Some(_) if ev.pressed_button != Some(MouseButton::Left) => self.tl_drag = None,
            Some(TlDrag::Seek) => self.seek_to(at),
            Some(TlDrag::Cut(i)) => self.move_cut(i, at),
            None => {}
        }
        cx.notify();
    }

    fn on_tl_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(TlDrag::Cut(i)) = self.tl_drag
            && let Some(c) = self.cuts.get(i)
        {
            log::info!("cut moved to {c:.1} s");
        }
        if let Some(before) = self.cut_drag_from.take()
            && before != self.cuts
        {
            self.undo.push((self.detections.clone(), self.deleted.clone(), before));
            if self.undo.len() > UNDO_STEPS {
                self.undo.remove(0);
            }
        }
        self.tl_drag = None;
        cx.notify();
    }

    // ------------------------------------------------------------ the bar

    pub(super) fn render_timeline_bar(&mut self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dur = self.duration();
        if dur <= 0.0 {
            return None;
        }
        let playing = self.is_playing();
        let opening = self.player_state == PlayerState::Opening;
        let pos = self.cursor_t.unwrap_or(0.0);
        let scene = self.timeline_scene(t);
        let rect = self.timeline_rect.clone();
        let hover = t.hover;
        let panel = self.bar_panel;
        let n_cuts = self.cuts.len();
        let w = self.center_w;
        let narrow = w < 640.0;
        let play = div()
            .id("play-btn")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(28.))
            .rounded(px(14.))
            .bg(t.accent)
            .cursor_pointer()
            .hover(|s| s.opacity(0.88))
            .child(icon(if playing { Icon::Pause } else { Icon::Play }, 12.0, t.accent_text))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_play(cx)));
        let time = mono(format!("{} / {}", crate::draw::fmt_time(pos, 1.0), crate::draw::fmt_time(dur, 1.0)))
            .flex_none()
            .w(px(if dur >= 3600.0 { 128. } else { 96. }))
            .text_size(px(theme::TEXT_XS))
            .text_color(if opening { t.text_faint } else { t.text_muted });
        let track = div()
            .id("timeline")
            .flex_1()
            .min_w(px(120.))
            .h(px(timeline::BAR_H - 4.0))
            .cursor(if self.tl_drag.is_some() { CursorStyle::ResizeLeftRight } else { CursorStyle::PointingHand })
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_tl_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_tl_right))
            .on_mouse_move(cx.listener(Self::on_tl_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_tl_up))
            .on_hover(cx.listener(|this, h: &bool, _, cx| {
                if !*h {
                    this.tl_hover = None;
                    cx.notify();
                }
            }))
            .child(canvas(move |b, _, _| rect.set(b), move |b, _, window, cx| timeline::paint(b, &scene, window, cx)).size_full());
        let boost_on = self.boost;
        let boost = switch(t, "boost-btn", if narrow { "Boost" } else { "Boost aircraft sound" }, boost_on).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.boost = !boost_on;
            if this.boost {
                let band = this.default_band();
                this.set_band(band, cx);
            }
            this.apply_listen();
            log::info!("boost: {} {:?}", this.boost, this.boost_band);
            cx.notify();
        }));
        let panel_btn = |id: &'static str, label: String, on: bool, p: BarPanel, cx: &mut Context<Self>| {
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
                .cursor_pointer()
                .text_color(if on { t.text } else { t.text_muted })
                .when(on, |d| d.bg(t.selected))
                .hover(move |s| s.bg(hover))
                .child(label)
                .child(icon(if on { Icon::ChevronDown } else { Icon::ChevronRight }, 10.0, t.text_muted))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.bar_panel = if this.bar_panel == Some(p) { None } else { Some(p) };
                    cx.notify();
                }))
        };
        let boost_more = panel_btn("boost-more", if narrow { String::new() } else { format!("{:.0}–{:.0} Hz", self.boost_band.0, self.boost_band.1) }, panel == Some(BarPanel::Boost), BarPanel::Boost, cx);
        let cuts_btn = panel_btn(
            "cuts-btn",
            match n_cuts {
                0 => "Cuts".to_string(),
                n => format!("Cuts ({n})"),
            },
            panel == Some(BarPanel::Cuts),
            BarPanel::Cuts,
            cx,
        )
        .child(icon(Icon::Scissors, 12.0, if n_cuts > 0 { t.danger } else { t.text_muted }));
        let controls = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(theme::SP2))
            .child(boost)
            .child(boost_more)
            .child(div().w(px(1.)).h(px(16.)).flex_none().bg(t.border))
            .child(cuts_btn);
        // a narrow middle: the listening and cut controls get their own row
        let two_rows = w > 0.0 && w < 700.0;
        let bar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(theme::SP2))
            .h(px(timeline::BAR_H))
            .px(px(theme::PANEL_PX))
            .bg(t.panel_bg)
            .child(play)
            .child(time)
            .child(track)
            .when(!two_rows, |d| d.child(div().w(px(1.)).h(px(16.)).flex_none().bg(t.border)));
        let (bar, second) = if two_rows {
            (bar, Some(div().flex().justify_end().px(px(theme::PANEL_PX)).pb(px(theme::SP1)).child(controls)))
        } else {
            (bar.child(controls), None)
        };
        let extra: Option<AnyElement> = match panel {
            Some(BarPanel::Boost) => Some(self.render_boost_row(t, cx)),
            Some(BarPanel::Cuts) => Some(self.render_cuts_row(t, cx)),
            None => None,
        };
        let failed = match &self.player_state {
            PlayerState::Failed(e) => Some(format!("No sound: {e}")),
            _ => None,
        };
        Some(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .border_t_1()
                .border_color(t.border)
                .child(bar)
                .children(second)
                .children(failed.map(|e| div().px(px(theme::PANEL_PX)).pb(px(theme::SP1)).text_size(px(theme::TEXT_XS)).text_color(t.danger).child(e)))
                .children(extra)
                .into_any_element(),
        )
    }

    fn render_boost_row(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let stepper = |id: &'static str, label: &str, v: f64, step: f64, lo: f64, hi: f64, which: u8, cx: &mut Context<Self>| {
            let set = move |this: &mut Self, nv: f64| {
                let nv = nv.clamp(lo, hi);
                if which == 0 {
                    this.boost_db = nv;
                } else {
                    this.volume_db = nv;
                }
                this.apply_listen();
            };
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(theme::SP1))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child(label.to_string()))
                .child(toggle(t, (id, 0usize), "−", false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    set(this, v - step);
                    cx.notify();
                })))
                .child(mono(format!("{v:+.0} dB")).w(px(48.)).flex().justify_center().text_size(px(theme::TEXT_XS)).text_color(t.text))
                .child(toggle(t, (id, 1usize), "+", false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    set(this, v + step);
                    cx.notify();
                })))
        };
        let field = |e: &Entity<TextInput>| div().w(px(64.)).flex_none().child(e.clone());
        let has_sel = self.selected.is_some();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(theme::SP3))
            .px(px(theme::PANEL_PX))
            .py(px(theme::SP2))
            .border_t_1()
            .border_color(t.border)
            .bg(t.surface)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(theme::SP1))
                    .child(div().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child("Band"))
                    .child(field(&self.boost_lo))
                    .child(div().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child("–"))
                    .child(field(&self.boost_hi))
                    .child(div().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child("Hz")),
            )
            .child(
                toggle(t, "band-from-sel", "Use the selected fly-by", false)
                    .when(!has_sel, |d| d.text_color(t.text_faint))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if has_sel {
                            let band = this.default_band();
                            this.set_band(band, cx);
                            this.boost = true;
                            this.apply_listen();
                        }
                        cx.notify();
                    })),
            )
            .child(stepper("boost-gain", "Boost", self.boost_db, 3.0, 0.0, 36.0, 0, cx))
            .child(stepper("volume", "Volume", self.volume_db, 3.0, -24.0, 24.0, 1, cx))
            .child(
                div()
                    .flex_1()
                    .min_w(px(160.))
                    .text_size(px(theme::TEXT_XS))
                    .text_color(t.text_faint)
                    .child("The band is raised and the rest lowered, to hear a faint propeller. Space plays and pauses."),
            )
            .into_any_element()
    }

    fn render_cuts_row(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let cursor = self.cursor_t;
        let can_add = cursor.is_some_and(|c| c > CUT_MIN_GAP && c < self.duration() - CUT_MIN_GAP && !self.cuts.iter().any(|&x| (x - c).abs() < CUT_MIN_GAP));
        let chips: Vec<AnyElement> = self
            .cuts
            .iter()
            .enumerate()
            .map(|(i, &c)| {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(theme::SP1))
                    .h(px(26.))
                    .pl(px(theme::SP2))
                    .rounded(px(theme::RADIUS))
                    .border_1()
                    .border_color(t.danger.opacity(0.6))
                    .child(icon(Icon::Scissors, 11.0, t.danger))
                    .child(mono(crate::draw::fmt_time(c, 1.0)).text_size(px(theme::TEXT_XS)).text_color(t.text))
                    .child(
                        div()
                            .id(("cut-x", i))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(20.))
                            .cursor_pointer()
                            .rounded(px(theme::RADIUS))
                            .hover(|s| s.opacity(0.7))
                            .child(icon(Icon::Close, 9.0, t.text_muted))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.remove_cut(i);
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        let sugg = self.open_suggestions();
        let n_sugg = sugg.len();
        let computing = self.cuts_task.is_some();
        let sugg_chips: Vec<AnyElement> = sugg
            .iter()
            .take(8)
            .enumerate()
            .map(|(i, s)| {
                let at = s.at;
                toggle(t, ("sugg", i), "", false)
                    .gap(px(theme::SP1))
                    .border_1()
                    .border_color(t.warn.opacity(0.6))
                    .h(px(26.))
                    .child(mono(crate::draw::fmt_time(at, 1.0)).text_size(px(theme::TEXT_XS)).text_color(t.warn))
                    .child(div().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child(s.kind.label()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.add_cut(at);
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        let can_undo = !self.undo.is_empty();
        let changed = self.cuts_changed() && matches!(self.location, LocState::Done(_));
        let can_search = changed && self.query(cx).is_ok() && !self.detections.is_empty();
        div()
            .flex()
            .flex_col()
            .gap(px(theme::SP2))
            .px(px(theme::PANEL_PX))
            .py(px(theme::SP2))
            .border_t_1()
            .border_color(t.border)
            .bg(t.surface)
            .child(
                div()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(t.text_muted)
                    .child("A cut is where part of the recording was removed and the rest joined: the time between fly-bys on either side no longer holds, so each part gets its own start time. Double-click the timeline to add one, drag it to move it, right-click it to remove it."),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(theme::SP2))
                    .child(
                        button(t, "cut-add", &match cursor {
                            Some(c) => format!("Cut at the cursor ({})", crate::draw::fmt_time(c, 1.0)),
                            None => "Cut at the cursor".to_string(),
                        }, Some(Icon::Scissors), false, can_add)
                        .when(can_add, |b| {
                            b.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if let Some(c) = this.cursor_t {
                                    this.add_cut(c);
                                }
                                cx.notify();
                            }))
                        }),
                    )
                    .children(chips)
                    .when(n_cuts_nonzero(&self.cuts), |d| {
                        d.child(toggle(t, "cuts-clear", "Remove all", false).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            if !this.cuts.is_empty() {
                                this.push_undo();
                                this.cuts.clear();
                                log::info!("all cuts removed");
                            }
                            cx.notify();
                        })))
                    })
                    .when(can_undo, |d| {
                        d.child(toggle(t, "cuts-undo", "Undo", false).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.do_undo();
                            cx.notify();
                        })))
                    })
                    .child(div().flex_1())
                    .when(changed, |d| {
                        d.child(
                            button(t, "cuts-search", "Search again", Some(Icon::Search), true, can_search)
                                .when(can_search, |b| b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.find_location(cx)))),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(theme::SP1))
                    .child(div().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).mr(px(theme::SP1)).child(if computing {
                        "Listening for possible cuts…".to_string()
                    } else if n_sugg == 0 && !self.cut_suggestions.is_empty() {
                        "You took every cut the audio suggests.".to_string()
                    } else if n_sugg == 0 {
                        "The audio shows no sign of a cut.".to_string()
                    } else {
                        format!("The audio may be cut at {n_sugg} place{} (dashed on the timeline; often only a bird or a gust). Click one to take it:", if n_sugg == 1 { "" } else { "s" })
                    }))
                    .children(sugg_chips),
            )
            .into_any_element()
    }
}

fn n_cuts_nonzero(c: &[f64]) -> bool {
    !c.is_empty()
}
