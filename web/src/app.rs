//! Emulator state in the browser: the machine, settings, audio/video output,
//! persistence (localStorage + IndexedDB) and the actions the UI triggers.

use std::cell::RefCell;
use std::rc::Rc;

use ps1wasm_core::pad::buttons;
use ps1wasm_core::Psx;
use wasm_bindgen::prelude::*;
use wasm_bindgen::{Clamped, JsCast};
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    AudioContext, AudioContextOptions, CanvasRenderingContext2d, GainNode, HtmlCanvasElement, HtmlSelectElement,
    IdbDatabase, ImageData,
};

use crate::idb;
use crate::logic::{self, Kind, KeyMap, Loaded};

use crate::dom::*;

pub(crate) const K_BIOS: &str = "psx-bios";
pub(crate) const K_BIOS_NAME: &str = "psx-bios-name";
pub(crate) const K_CARD: [&str; 2] = ["psx-memcard-0", "psx-memcard-1"];
pub(crate) const K_KEYS: &str = "psx-keys";
pub(crate) const K_SETTINGS: &str = "psx-settings";
pub(crate) const AUDIO_RATE: f32 = 44100.0;
pub(crate) const AUDIO_MAX_AHEAD: f64 = 0.20;
pub(crate) const AUDIO_LEAD: f64 = 0.05;


// ------------------------------------------------------------------ settings
#[derive(Clone)]
pub(crate) struct Settings {
    pub(crate) smooth: bool,
    pub(crate) stretch: bool,
    pub(crate) dualshock: bool,
    pub(crate) volume: u32,
    pub(crate) remember: bool,
}

impl Settings {
    pub(crate) fn load() -> Settings {
        let s = storage().and_then(|st| st.get_item(K_SETTINGS).ok().flatten()).unwrap_or_default();
        let get = |k: &str| s.split(';').find_map(|kv| kv.strip_prefix(&format!("{}=", k)).map(str::to_string));
        Settings {
            smooth: get("smooth").as_deref() == Some("1"),
            stretch: get("stretch").as_deref() == Some("1"),
            dualshock: get("dualshock").as_deref() != Some("0"),
            volume: get("volume").and_then(|v| v.parse().ok()).unwrap_or(80),
            remember: get("remember").as_deref() != Some("0"),
        }
    }
    pub(crate) fn save(&self) {
        let s = format!(
            "smooth={};stretch={};dualshock={};volume={};remember={}",
            self.smooth as u8, self.stretch as u8, self.dualshock as u8, self.volume, self.remember as u8
        );
        if let Some(st) = storage() {
            let _ = st.set_item(K_SETTINGS, &s);
        }
    }
}

// ------------------------------------------------------------------ app state
pub(crate) struct App {
    pub(crate) psx: Psx,
    pub(crate) bios: Option<(String, Vec<u8>)>,
    pub(crate) game: Option<(Loaded, String)>,
    pub(crate) title: Option<String>,
    pub(crate) running: bool,
    pub(crate) paused: bool,
    pub(crate) keys: u16,
    /// Buttons held on the on-screen touch controls.
    pub(crate) touch: u16,
    pub(crate) keymap: KeyMap,
    pub(crate) listening: Option<u16>,
    pub(crate) fast: bool,
    pub(crate) settings: Settings,
    pub(crate) frame_buf: Vec<u8>,
    pub(crate) audio_buf: Vec<i16>,
    pub(crate) audio: Option<(AudioContext, GainNode)>,
    pub(crate) audio_time: f64,
    pub(crate) last_time: f64,
    pub(crate) acc: f64,
    pub(crate) fps_frames: u32,
    pub(crate) fps_time: f64,
    pub(crate) emu_ms: f64,
    pub(crate) save_timer: u32,
    pub(crate) db: Option<IdbDatabase>,
    pub(crate) canvas: HtmlCanvasElement,
    pub(crate) ctx: CanvasRenderingContext2d,
    pub(crate) led: bool,
}

pub(crate) type Shared = Rc<RefCell<App>>;

impl App {
    /// Fresh state: settings and key bindings come from localStorage.
    pub(crate) fn new(canvas: HtmlCanvasElement) -> App {
        let ctx: CanvasRenderingContext2d = canvas.get_context("2d").unwrap().unwrap().dyn_into().unwrap();
        let keymap = storage().and_then(|s| s.get_item(K_KEYS).ok().flatten()).map(|s| KeyMap::parse(&s)).unwrap_or_default();
        App {
            psx: Psx::new(),
            bios: None,
            game: None,
            title: None,
            running: false,
            paused: false,
            keys: 0,
            touch: 0,
            keymap,
            listening: None,
            fast: false,
            settings: Settings::load(),
            frame_buf: vec![],
            audio_buf: vec![],
            audio: None,
            audio_time: 0.0,
            last_time: 0.0,
            acc: 0.0,
            fps_frames: 0,
            fps_time: 0.0,
            emu_ms: 0.0,
            save_timer: 0,
            db: None,
            canvas,
            ctx,
            led: false,
        }
    }

    // ---------------------------------------------------------- audio/video
    pub(crate) fn ensure_audio(&mut self) {
        if self.audio.is_none() {
            let opts = AudioContextOptions::new();
            opts.set_sample_rate(AUDIO_RATE);
            if let Ok(ctx) = AudioContext::new_with_context_options(&opts) {
                if let Ok(gain) = ctx.create_gain() {
                    let _ = gain.connect_with_audio_node(&ctx.destination());
                    self.audio = Some((ctx, gain));
                    self.apply_volume();
                }
            }
        }
        if let Some((ctx, _)) = &self.audio {
            let _ = ctx.resume();
        }
    }

    pub(crate) fn apply_volume(&self) {
        if let Some((_, gain)) = &self.audio {
            let v = self.settings.volume as f32 / 100.0;
            gain.gain().set_value(v * v);
        }
    }

    pub(crate) fn push_audio(&mut self) {
        self.audio_buf.clear();
        self.psx.take_audio(&mut self.audio_buf);
        let Some((ctx, gain)) = &self.audio else { return };
        let frames = self.audio_buf.len() / 2;
        if frames == 0 || self.fast {
            return;
        }
        let t = ctx.current_time();
        if self.audio_time < t + 0.01 {
            self.audio_time = t + AUDIO_LEAD;
        }
        if self.audio_time > t + AUDIO_MAX_AHEAD {
            return;
        }
        let Ok(buf) = ctx.create_buffer(2, frames as u32, AUDIO_RATE) else { return };
        let (mut l, mut r) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
        for &[sl, sr] in self.audio_buf.as_chunks::<2>().0 {
            l.push(sl as f32 / 32768.0);
            r.push(sr as f32 / 32768.0);
        }
        let _ = buf.copy_to_channel(&l, 0);
        let _ = buf.copy_to_channel(&r, 1);
        let Ok(src) = ctx.create_buffer_source() else { return };
        src.set_buffer(Some(&buf));
        if src.connect_with_audio_node(gain).is_ok() {
            let _ = src.start_with_when(self.audio_time);
            self.audio_time += frames as f64 / AUDIO_RATE as f64;
        }
    }

    pub(crate) fn draw(&mut self) {
        let (w, h) = self.psx.render(&mut self.frame_buf);
        if self.canvas.width() != w || self.canvas.height() != h {
            self.canvas.set_width(w);
            self.canvas.set_height(h);
        }
        if let Ok(img) = ImageData::new_with_u8_clamped_array_and_sh(Clamped(&self.frame_buf), w, h) {
            let _ = self.ctx.put_image_data(&img, 0.0, 0.0);
        }
    }

    // ---------------------------------------------------------- input
    pub(crate) fn poll_gamepads(&mut self) -> u16 {
        let mut b = 0u16;
        let Ok(pads) = window().navigator().get_gamepads() else { return 0 };
        for p in pads.iter() {
            let Ok(gp) = p.dyn_into::<web_sys::Gamepad>() else { continue };
            for (i, btn) in gp.buttons().iter().enumerate() {
                if let Ok(btn) = btn.dyn_into::<web_sys::GamepadButton>() {
                    if btn.pressed() {
                        b |= logic::gamepad_button(i).unwrap_or(0);
                    }
                }
            }
            let axes = gp.axes();
            let ax = |i: u32| axes.get(i).as_f64().unwrap_or(0.0);
            let to_u8 = |v: f64| (((v.clamp(-1.0, 1.0) + 1.0) * 127.5).round() as i32).clamp(0, 255) as u8;
            let pad = &mut self.psx.bus.sio.ports[0].pad;
            if axes.length() >= 4 {
                pad.sticks = [to_u8(ax(2)), to_u8(ax(3)), to_u8(ax(0)), to_u8(ax(1))];
            }
            if !pad.analog && axes.length() >= 2 {
                b |= logic::stick_to_dpad(ax(0), ax(1));
            }
            set_text("gamepad-status", &format!("Connected: {}", gp.id()));
            break;
        }
        b
    }

    // ---------------------------------------------------------- frame loop
    pub(crate) fn frame(&mut self, t: f64) {
        let dt = (t - self.last_time).min(100.0);
        self.last_time = t;
        if !self.running || self.paused {
            return;
        }
        let pad = self.keys | self.touch | self.poll_gamepads();
        self.psx.set_buttons(0, pad);
        let hz = if self.psx.bus.gpu.pal { 50.0 } else { 59.29 };
        let frame_ms = 1000.0 / hz;
        self.acc += dt;
        let budget = if self.fast { 4 } else { 3 };
        let mut n = 0;
        while (self.acc >= frame_ms || (self.fast && n < budget)) && n < budget {
            let t0 = now();
            self.psx.run_frame();
            self.emu_ms += now() - t0;
            self.acc = (self.acc - frame_ms).max(0.0);
            n += 1;
            self.fps_frames += 1;
        }
        if n == budget {
            self.acc = 0.0;
        }
        if n > 0 {
            self.draw();
            self.push_audio();
        }
        let led = self.psx.bus.sio.ports[0].pad.analog;
        if led != self.led {
            self.led = led;
            set_class("led", "on", led);
        }
        if t - self.fps_time >= 1000.0 {
            let fps = self.fps_frames as f64 * 1000.0 / (t - self.fps_time);
            let ms = if self.fps_frames > 0 { self.emu_ms / self.fps_frames as f64 } else { 0.0 };
            set_text("fps", &format!("{:.0} fps · {:.1} ms/frame{}", fps, ms, if self.fast { " · ⏩" } else { "" }));
            self.fps_frames = 0;
            self.emu_ms = 0.0;
            self.fps_time = t;
        }
        self.save_timer += 1;
        if self.save_timer >= 120 {
            self.save_timer = 0;
            self.save_cards(false);
        }
    }

    // ---------------------------------------------------------- persistence
    pub(crate) fn save_cards(&mut self, force: bool) {
        let Some(st) = storage() else { return };
        for (port, key) in K_CARD.iter().enumerate() {
            if self.psx.memcard_take_dirty(port) || force {
                if let Some(data) = self.psx.memcard(port) {
                    if st.set_item(key, &logic::b64_encode(data)).is_err() {
                        toast("Could not save memory card (browser storage full?)", "bad");
                    }
                }
            }
        }
    }

    pub(crate) fn restore_cards(&mut self) {
        let Some(st) = storage() else { return };
        for (port, key) in K_CARD.iter().enumerate() {
            if let Ok(Some(s)) = st.get_item(key) {
                if let Some(data) = logic::b64_decode(&s) {
                    self.psx.set_memcard(port, data);
                }
            }
        }
    }

    pub(crate) fn set_bios(&mut self, name: &str, data: Vec<u8>, persist: bool) -> bool {
        if let Err(e) = self.psx.load_bios(&data) {
            toast(&format!("BIOS rejected: {}", e), "bad");
            return false;
        }
        let desc = logic::bios_description(&data);
        set_text("bios-status", &format!("{} — {}", name, desc));
        set_class("step-bios", "done", true);
        if persist {
            if let Some(st) = storage() {
                let ok = st.set_item(K_BIOS, &logic::b64_encode(&data)).is_ok();
                let _ = st.set_item(K_BIOS_NAME, name);
                if !ok {
                    toast("BIOS loaded, but couldn't be remembered (storage full)", "bad");
                }
            }
        }
        self.bios = Some((name.to_string(), data));
        self.update_start_buttons();
        true
    }

    pub(crate) fn update_start_buttons(&self) {
        let have_bios = self.bios.is_some();
        let exe = matches!(self.game, Some((Loaded::Exe(..), _)));
        set_disabled("start-btn", !(self.game.is_some() && (have_bios || exe)));
        set_disabled("bios-only-btn", !have_bios);
    }

    pub(crate) fn apply_pad_type(&mut self) {
        let ds = self.settings.dualshock;
        for p in self.psx.bus.sio.ports.iter_mut() {
            p.pad.dualshock = ds;
            if !ds {
                p.pad.analog = false;
            }
        }
        set_class("btn-analog", "hidden", !ds);
    }

    // ---------------------------------------------------------- lifecycle
    /// Fresh machine: BIOS + cards + settings (+ game if any). Starts running.
    pub(crate) fn boot(&mut self, with_game: bool) {
        self.psx = Psx::new();
        if let Some((_, b)) = &self.bios {
            let _ = self.psx.load_bios(b);
        }
        self.restore_cards();
        self.apply_pad_type();
        if with_game {
            match self.game.take() {
                Some((Loaded::Disc(disc, name), _)) => {
                    self.psx.insert_disc(disc);
                    self.title = Some(name);
                }
                Some((Loaded::Exe(exe, name), _)) => {
                    let r = if self.bios.is_some() { self.psx.load_exe_after_bios(&exe) } else { self.psx.load_exe_now(&exe) };
                    if let Err(e) = r {
                        toast(&format!("Can't run EXE: {}", e), "bad");
                        return;
                    }
                    // keep the EXE so Reset can restart it
                    self.game = Some((Loaded::Exe(exe, name.clone()), name.clone()));
                    self.title = Some(name);
                }
                None => {}
            }
        } else {
            self.title = None;
        }
        self.running = true;
        self.paused = false;
        self.ensure_audio();
        show("start", false);
        show("toolbar", true);
        let _ = document().body().unwrap().class_list().add_1("playing");
        show("paused", false);
        set_text("btn-pause", "⏸ Pause");
        let what = self.title.clone().unwrap_or_else(|| "BIOS menu".into());
        document().set_title(&format!("{} — ps1wasm", what));
        toast(&format!("Starting {}", what), "ok");
    }

    pub(crate) fn eject(&mut self) {
        self.save_cards(true);
        self.running = false;
        self.psx = Psx::new();
        if let Some((_, b)) = &self.bios {
            let _ = self.psx.load_bios(b);
        }
        self.game = None;
        self.title = None;
        set_class("step-game", "done", false);
        set_text("game-status", ".cue with its .bin file(s), a .bin/.iso image, or a PS-X executable (.exe / .ps-exe)");
        show("start", true);
        show("toolbar", false);
        let _ = document().body().unwrap().class_list().remove_2("playing", "menu-open");
        show("paused", false);
        set_text("fps", "");
        document().set_title("ps1wasm — PS1 emulator in your browser");
        self.update_start_buttons();
        // blank the screen
        self.ctx.set_fill_style_str("#000");
        self.ctx.fill_rect(0.0, 0.0, self.canvas.width() as f64, self.canvas.height() as f64);
    }

    pub(crate) fn set_paused(&mut self, p: bool) {
        if !self.running {
            return;
        }
        self.paused = p;
        show("paused", p);
        set_text("btn-pause", if p { "▶ Resume" } else { "⏸ Pause" });
        if let Some((ctx, _)) = &self.audio {
            let _ = if p { ctx.suspend() } else { ctx.resume() };
        }
    }

    pub(crate) fn state_key(&self, slot: &str) -> String {
        format!("state/{}/{}", self.title.as_deref().unwrap_or("bios"), slot)
    }

    pub(crate) fn render_keymap(app: &Shared) {
        let tbody = el("keymap");
        tbody.set_inner_html("");
        let a = app.borrow();
        for (bit, name) in logic::BUTTONS {
            let tr = document().create_element("tr").unwrap();
            let td1 = document().create_element("td").unwrap();
            td1.set_text_content(Some(name));
            let td2 = document().create_element("td").unwrap();
            let b = document().create_element("button").unwrap();
            b.set_class_name(if a.listening == Some(bit) { "btn listening" } else { "btn" });
            let label = if a.listening == Some(bit) { "Press a key…".to_string() } else { logic::key_label(a.keymap.key_for(bit)) };
            b.set_text_content(Some(&label));
            let app2 = app.clone();
            on(&b, "click", move |_| {
                app2.borrow_mut().listening = Some(bit);
                App::render_keymap(&app2);
            });
            td2.append_child(&b).unwrap();
            tr.append_child(&td1).unwrap();
            tr.append_child(&td2).unwrap();
            tbody.append_child(&tr).unwrap();
        }
        drop(a);
        App::render_help(app);
    }

    pub(crate) fn render_help(app: &Shared) {
        let a = app.borrow();
        let k = |b: u16| logic::key_label(a.keymap.key_for(b));
        let html = format!(
            "<span><kbd>{}</kbd><kbd>{}</kbd><kbd>{}</kbd><kbd>{}</kbd> D-pad</span>\
             <span><kbd>{}</kbd> ✕ <kbd>{}</kbd> ◯ <kbd>{}</kbd> □ <kbd>{}</kbd> △</span>\
             <span><kbd>{}</kbd> Start <kbd>{}</kbd> Select</span>\
             <span><kbd>{}</kbd>/<kbd>{}</kbd> L1/R1 <kbd>{}</kbd>/<kbd>{}</kbd> L2/R2</span>\
             <span><kbd>F5</kbd> save <kbd>F9</kbd> load <kbd>Esc</kbd> pause <kbd>Tab</kbd> fast-forward <kbd>F2</kbd> analog</span>\
             <span>Gamepads work too.</span>",
            k(buttons::UP), k(buttons::DOWN), k(buttons::LEFT), k(buttons::RIGHT),
            k(buttons::CROSS), k(buttons::CIRCLE), k(buttons::SQUARE), k(buttons::TRIANGLE),
            k(buttons::START), k(buttons::SELECT), k(buttons::L1), k(buttons::R1), k(buttons::L2), k(buttons::R2)
        );
        el("help").set_inner_html(&html);
    }
}

// ------------------------------------------------------------------ actions
pub(crate) async fn save_state(app: Shared) {
    let (key, data, db) = {
        let mut a = app.borrow_mut();
        if !a.running {
            return;
        }
        let slot = el("slot").dyn_into::<HtmlSelectElement>().unwrap().value();
        (a.state_key(&slot), a.psx.save_state(), a.db.clone())
    };
    let Some(db) = db else { return toast("Save states need IndexedDB (unavailable)", "bad") };
    match idb::put_bytes(&db, &key, &data).await {
        Ok(()) => toast(&format!("Saved to slot {}", key.rsplit('/').next().unwrap_or("?")), "ok"),
        Err(_) => toast("Saving failed (storage full?)", "bad"),
    }
}

pub(crate) async fn load_state(app: Shared) {
    let (key, db) = {
        let a = app.borrow();
        if !a.running {
            return;
        }
        let slot = el("slot").dyn_into::<HtmlSelectElement>().unwrap().value();
        (a.state_key(&slot), a.db.clone())
    };
    let Some(db) = db else { return };
    match idb::get_bytes(&db, &key).await {
        None => toast("That slot is empty", "bad"),
        Some(data) => {
            let mut a = app.borrow_mut();
            match a.psx.load_state(&data) {
                Ok(()) => {
                    a.audio_time = 0.0;
                    a.draw();
                    toast(&format!("Loaded slot {}", key.rsplit('/').next().unwrap_or("?")), "ok");
                }
                Err(e) => toast(&format!("Can't load state: {}", e), "bad"),
            }
        }
    }
}

/// Remember game files for "Continue" (copied into JS, written async).
pub(crate) fn remember_game(app: &Shared, title: &str, files: &[(String, Vec<u8>)]) {
    let Some(db) = app.borrow().db.clone() else { return };
    let total: usize = files.iter().map(|(_, d)| d.len()).sum();
    if total > 1_200_000_000 {
        return;
    }
    let names: Vec<String> = files.iter().map(|(n, _)| n.clone()).collect();
    let arrays: Vec<js_sys::Uint8Array> = files.iter().map(|(_, d)| js_sys::Uint8Array::from(&d[..])).collect();
    let title = title.to_string();
    spawn_local(async move {
        let _ = idb::put(&db, "last/title", &JsValue::from_str("")).await;
        for (n, arr) in names.iter().zip(arrays) {
            if idb::put(&db, &format!("last/file/{}", n), &arr.into()).await.is_err() {
                return;
            }
        }
        let _ = idb::put(&db, "last/names", &JsValue::from_str(&names.join("\n"))).await;
        let _ = idb::put(&db, "last/title", &JsValue::from_str(&title)).await;
    });
}

pub(crate) async fn forget_game(db: &IdbDatabase) {
    if let Some(names) = idb::get_string(db, "last/names").await {
        for n in names.lines() {
            let _ = idb::delete(db, &format!("last/file/{}", n)).await;
        }
    }
    let _ = idb::delete(db, "last/names").await;
    let _ = idb::delete(db, "last/title").await;
}

/// Handle any set of files (picked or dropped).
pub(crate) fn handle_files(app: &Shared, files: Vec<(String, Vec<u8>)>, force_bios: bool) {
    let mut game_files = vec![];
    for (name, data) in files {
        let kind = if force_bios { Kind::Bios } else { logic::classify(&name, data.len()) };
        match kind {
            Kind::Bios => {
                let mut a = app.borrow_mut();
                if a.set_bios(&name, data, true) {
                    toast(&format!("BIOS ready: {}", name), "ok");
                }
            }
            Kind::MemCard => match logic::import_memcard(&data) {
                Ok(raw) => {
                    let mut a = app.borrow_mut();
                    a.psx.set_memcard(0, raw);
                    a.save_cards(true);
                    toast(&format!("Memory card {} imported into slot 1", name), "ok");
                }
                Err(e) => toast(&format!("{}: {}", name, e), "bad"),
            },
            Kind::State => {
                let mut a = app.borrow_mut();
                if !a.running {
                    toast("Start the game first, then drop its save state", "bad");
                } else {
                    match a.psx.load_state(&data) {
                        Ok(()) => toast("Save state loaded", "ok"),
                        Err(e) => toast(&format!("Can't load state: {}", e), "bad"),
                    }
                }
            }
            Kind::Game => game_files.push((name, data)),
        }
    }
    if game_files.is_empty() {
        maybe_autostart(app);
        return;
    }
    set_text("game-status", "Loading…");
    let remember = app.borrow().settings.remember;
    let title_guess = game_files.iter().find(|(n, _)| n.to_lowercase().ends_with(".cue")).or(game_files.first()).map(|(n, _)| n.clone()).unwrap_or_default();
    if remember {
        remember_game(app, &title_guess, &game_files);
    }
    match logic::load_files(game_files) {
        Ok(loaded) => {
            let name = match &loaded {
                Loaded::Disc(_, n) | Loaded::Exe(_, n) => n.clone(),
            };
            set_text("game-status", &name);
            set_class("step-game", "done", true);
            let mut a = app.borrow_mut();
            if a.running {
                // a game was already running: switch to the new one
                a.running = false;
            }
            a.game = Some((loaded, name));
            a.update_start_buttons();
            drop(a);
            maybe_autostart(app);
        }
        Err(e) => {
            set_text("game-status", &format!("⚠ {}", e));
            set_class("step-game", "done", false);
            toast(&e, "bad");
        }
    }
}

pub(crate) fn maybe_autostart(app: &Shared) {
    let mut a = app.borrow_mut();
    if a.running || a.game.is_none() {
        return;
    }
    let exe = matches!(a.game, Some((Loaded::Exe(..), _)));
    if a.bios.is_some() || exe {
        a.boot(true);
    } else {
        toast("Now add your BIOS to start", "");
    }
}
