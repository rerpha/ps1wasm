//! Browser frontend, written in Rust (wasm-bindgen + web-sys).
//!
//! The page layout lives in static/index.html; this module wires it up.
//! The only JavaScript involved is the module loader in index.html.

pub mod logic;

#[cfg(target_arch = "wasm32")]
mod idb;

#[cfg(target_arch = "wasm32")]
mod app {
    use std::cell::RefCell;
    use std::rc::Rc;

    use ps1wasm_core::pad::{buttons, MemCard};
    use ps1wasm_core::Psx;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::{Clamped, JsCast};
    use wasm_bindgen_futures::{spawn_local, JsFuture};
    use web_sys::{
        AudioContext, AudioContextOptions, CanvasRenderingContext2d, Document, DragEvent, Element, Event, FileList,
        GainNode, HtmlButtonElement, HtmlCanvasElement, HtmlDialogElement, HtmlInputElement, HtmlSelectElement,
        IdbDatabase, ImageData, KeyboardEvent, Storage, Window,
    };

    use crate::idb;
    use crate::logic::{self, Kind, KeyMap, Loaded};

    const K_BIOS: &str = "psx-bios";
    const K_BIOS_NAME: &str = "psx-bios-name";
    const K_CARD: [&str; 2] = ["psx-memcard-0", "psx-memcard-1"];
    const K_KEYS: &str = "psx-keys";
    const K_SETTINGS: &str = "psx-settings";
    const AUDIO_RATE: f32 = 44100.0;
    const AUDIO_MAX_AHEAD: f64 = 0.20;
    const AUDIO_LEAD: f64 = 0.05;

    // ------------------------------------------------------------------ dom helpers
    fn window() -> Window {
        web_sys::window().expect("no window")
    }
    fn document() -> Document {
        window().document().expect("no document")
    }
    fn storage() -> Option<Storage> {
        window().local_storage().ok().flatten()
    }
    fn el(id: &str) -> Element {
        document().get_element_by_id(id).unwrap_or_else(|| panic!("#{} missing", id))
    }
    fn input(id: &str) -> HtmlInputElement {
        el(id).dyn_into().unwrap()
    }
    fn set_text(id: &str, t: &str) {
        el(id).set_text_content(Some(t));
    }
    fn show(id: &str, on: bool) {
        let _ = el(id).class_list().toggle_with_force("hidden", !on);
    }
    fn set_class(id: &str, class: &str, on: bool) {
        let _ = el(id).class_list().toggle_with_force(class, on);
    }
    fn set_disabled(id: &str, d: bool) {
        if let Ok(b) = el(id).dyn_into::<HtmlButtonElement>() {
            b.set_disabled(d);
        }
    }
    fn on<F: FnMut(Event) + 'static>(target: &web_sys::EventTarget, ev: &str, f: F) {
        let c = Closure::<dyn FnMut(Event)>::new(f);
        target.add_event_listener_with_callback(ev, c.as_ref().unchecked_ref()).unwrap();
        c.forget();
    }
    fn on_id<F: FnMut(Event) + 'static>(id: &str, ev: &str, f: F) {
        on(&el(id), ev, f);
    }
    fn now() -> f64 {
        window().performance().map(|p| p.now()).unwrap_or(0.0)
    }

    pub fn toast(msg: &str, kind: &str) {
        let t = document().create_element("div").unwrap();
        t.set_class_name(&format!("toast {}", kind));
        t.set_text_content(Some(msg));
        el("toasts").append_child(&t).unwrap();
        let t2 = t.clone();
        let cb = Closure::once_into_js(move || t2.remove());
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), 3200);
    }

    fn download(name: &str, data: &[u8]) {
        let arr = js_sys::Array::new();
        arr.push(&js_sys::Uint8Array::from(data));
        let opts = web_sys::BlobPropertyBag::new();
        opts.set_type("application/octet-stream");
        let Ok(blob) = web_sys::Blob::new_with_u8_array_sequence_and_options(&arr, &opts) else { return };
        let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else { return };
        let a: web_sys::HtmlAnchorElement = document().create_element("a").unwrap().dyn_into().unwrap();
        a.set_href(&url);
        a.set_download(name);
        a.click();
        let _ = web_sys::Url::revoke_object_url(&url);
    }

    /// Fetch a file shipped with the site (relative URL).
    async fn fetch_bytes(url: &str) -> Option<Vec<u8>> {
        let resp: web_sys::Response = JsFuture::from(window().fetch_with_str(url)).await.ok()?.dyn_into().ok()?;
        if !resp.ok() {
            return None;
        }
        let buf = JsFuture::from(resp.array_buffer().ok()?).await.ok()?;
        Some(js_sys::Uint8Array::new(&buf).to_vec())
    }

    async fn read_list(list: Option<FileList>) -> Vec<(String, Vec<u8>)> {
        let mut out = vec![];
        let Some(list) = list else { return out };
        for i in 0..list.length() {
            let Some(f) = list.get(i) else { continue };
            match JsFuture::from(f.array_buffer()).await {
                Ok(buf) => out.push((f.name(), js_sys::Uint8Array::new(&buf).to_vec())),
                Err(_) => toast(&format!("Could not read {}", f.name()), "bad"),
            }
        }
        out
    }

    // ------------------------------------------------------------------ settings
    #[derive(Clone)]
    struct Settings {
        smooth: bool,
        stretch: bool,
        dualshock: bool,
        volume: u32,
        remember: bool,
    }

    impl Settings {
        fn load() -> Settings {
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
        fn save(&self) {
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
    struct App {
        psx: Psx,
        bios: Option<(String, Vec<u8>)>,
        game: Option<(Loaded, String)>,
        title: Option<String>,
        running: bool,
        paused: bool,
        keys: u16,
        /// Buttons held on the on-screen touch controls.
        touch: u16,
        keymap: KeyMap,
        listening: Option<u16>,
        fast: bool,
        settings: Settings,
        frame_buf: Vec<u8>,
        audio_buf: Vec<i16>,
        audio: Option<(AudioContext, GainNode)>,
        audio_time: f64,
        last_time: f64,
        acc: f64,
        fps_frames: u32,
        fps_time: f64,
        emu_ms: f64,
        save_timer: u32,
        db: Option<IdbDatabase>,
        canvas: HtmlCanvasElement,
        ctx: CanvasRenderingContext2d,
        led: bool,
    }

    type Shared = Rc<RefCell<App>>;

    impl App {
        // ---------------------------------------------------------- audio/video
        fn ensure_audio(&mut self) {
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

        fn apply_volume(&self) {
            if let Some((_, gain)) = &self.audio {
                let v = self.settings.volume as f32 / 100.0;
                gain.gain().set_value(v * v);
            }
        }

        fn push_audio(&mut self) {
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
            for s in self.audio_buf.chunks_exact(2) {
                l.push(s[0] as f32 / 32768.0);
                r.push(s[1] as f32 / 32768.0);
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

        fn draw(&mut self) {
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
        fn poll_gamepads(&mut self) -> u16 {
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
        fn frame(&mut self, t: f64) {
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
        fn save_cards(&mut self, force: bool) {
            let Some(st) = storage() else { return };
            for port in 0..2 {
                if self.psx.memcard_take_dirty(port) || force {
                    if let Some(data) = self.psx.memcard(port) {
                        if st.set_item(K_CARD[port], &logic::b64_encode(data)).is_err() {
                            toast("Could not save memory card (browser storage full?)", "bad");
                        }
                    }
                }
            }
        }

        fn restore_cards(&mut self) {
            let Some(st) = storage() else { return };
            for port in 0..2 {
                if let Ok(Some(s)) = st.get_item(K_CARD[port]) {
                    if let Some(data) = logic::b64_decode(&s) {
                        self.psx.set_memcard(port, data);
                    }
                }
            }
        }

        fn set_bios(&mut self, name: &str, data: Vec<u8>, persist: bool) -> bool {
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

        fn update_start_buttons(&self) {
            let have_bios = self.bios.is_some();
            let exe = matches!(self.game, Some((Loaded::Exe(..), _)));
            set_disabled("start-btn", !(self.game.is_some() && (have_bios || exe)));
            set_disabled("bios-only-btn", !have_bios);
        }

        fn apply_pad_type(&mut self) {
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
        fn boot(&mut self, with_game: bool) {
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
            show("paused", false);
            set_text("btn-pause", "⏸ Pause");
            let what = self.title.clone().unwrap_or_else(|| "BIOS menu".into());
            document().set_title(&format!("{} — ps1wasm", what));
            toast(&format!("Starting {}", what), "ok");
        }

        fn eject(&mut self) {
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
            show("paused", false);
            set_text("fps", "");
            document().set_title("ps1wasm — PS1 emulator in your browser");
            self.update_start_buttons();
            // blank the screen
            self.ctx.set_fill_style_str("#000");
            self.ctx.fill_rect(0.0, 0.0, self.canvas.width() as f64, self.canvas.height() as f64);
        }

        fn set_paused(&mut self, p: bool) {
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

        fn state_key(&self, slot: &str) -> String {
            format!("state/{}/{}", self.title.as_deref().unwrap_or("bios"), slot)
        }

        fn render_keymap(app: &Shared) {
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

        fn render_help(app: &Shared) {
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
    async fn save_state(app: Shared) {
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

    async fn load_state(app: Shared) {
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
    fn remember_game(app: &Shared, title: &str, files: &[(String, Vec<u8>)]) {
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

    async fn forget_game(db: &IdbDatabase) {
        if let Some(names) = idb::get_string(db, "last/names").await {
            for n in names.lines() {
                let _ = idb::delete(db, &format!("last/file/{}", n)).await;
            }
        }
        let _ = idb::delete(db, "last/names").await;
        let _ = idb::delete(db, "last/title").await;
    }

    /// Handle any set of files (picked or dropped).
    fn handle_files(app: &Shared, files: Vec<(String, Vec<u8>)>, force_bios: bool) {
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

    fn maybe_autostart(app: &Shared) {
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

    // ------------------------------------------------------------------ entry
    #[wasm_bindgen(start)]
    pub fn start() {
        std::panic::set_hook(Box::new(|info| {
            web_sys::console::error_1(&info.to_string().into());
            toast(&format!("Emulator crashed: {}", info), "bad");
        }));

        let canvas: HtmlCanvasElement = el("screen").dyn_into().unwrap();
        let ctx: CanvasRenderingContext2d = canvas.get_context("2d").unwrap().unwrap().dyn_into().unwrap();
        let settings = Settings::load();
        let keymap = storage().and_then(|s| s.get_item(K_KEYS).ok().flatten()).map(|s| KeyMap::parse(&s)).unwrap_or_default();
        let app: Shared = Rc::new(RefCell::new(App {
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
            settings: settings.clone(),
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
        }));

        // ---- restore settings into the UI
        input("opt-smooth").set_checked(settings.smooth);
        input("opt-remember").set_checked(settings.remember);
        input("volume").set_value(&settings.volume.to_string());
        el("opt-aspect").dyn_into::<HtmlSelectElement>().unwrap().set_value(if settings.stretch { "stretch" } else { "4:3" });
        el("opt-pad").dyn_into::<HtmlSelectElement>().unwrap().set_value(if settings.dualshock { "dualshock" } else { "digital" });
        set_class("screen-wrap", "smooth", settings.smooth);
        set_class("screen-wrap", "stretch", settings.stretch);
        App::render_help(&app);

        // ---- remembered BIOS
        if let Some(st) = storage() {
            if let (Ok(Some(b64)), name) = (st.get_item(K_BIOS), st.get_item(K_BIOS_NAME).ok().flatten()) {
                if let Some(data) = logic::b64_decode(&b64) {
                    let name = name.unwrap_or_else(|| "BIOS".into());
                    app.borrow_mut().set_bios(&format!("{} (remembered)", name), data, false);
                }
            }
        }
        app.borrow_mut().update_start_buttons();

        // ---- IndexedDB (states + last game)
        {
            let app = app.clone();
            spawn_local(async move {
                match idb::open().await {
                    Ok(db) => {
                        if let Some(t) = idb::get_string(&db, "last/title").await.filter(|t| !t.is_empty()) {
                            set_text("continue-name", &t);
                            show("continue", true);
                        }
                        app.borrow_mut().db = Some(db);
                    }
                    Err(_) => web_sys::console::warn_1(&"IndexedDB unavailable: no save states".into()),
                }
            });
        }

        // ---- start card
        {
            let app2 = app.clone();
            let inp = input("bios-input");
            on(&inp.clone(), "change", move |_| {
                let app = app2.clone();
                let inp = inp.clone();
                spawn_local(async move {
                    let files = read_list(inp.files()).await;
                    inp.set_value("");
                    handle_files(&app, files, true);
                });
            });
        }
        {
            let app2 = app.clone();
            let inp = input("game-input");
            on(&inp.clone(), "change", move |_| {
                let app = app2.clone();
                let inp = inp.clone();
                spawn_local(async move {
                    set_text("game-status", "Reading files…");
                    let files = read_list(inp.files()).await;
                    inp.set_value("");
                    handle_files(&app, files, false);
                });
            });
        }
        {
            let app = app.clone();
            on_id("start-btn", "click", move |_| app.borrow_mut().boot(true));
        }
        {
            let app = app.clone();
            on_id("bios-only-btn", "click", move |_| {
                let mut a = app.borrow_mut();
                a.game = None;
                a.boot(false);
            });
        }
        {
            let app2 = app.clone();
            on_id("openbios-btn", "click", move |_| {
                let app = app2.clone();
                spawn_local(async move {
                    match fetch_bytes("openbios/openbios.bin").await {
                        Some(data) => {
                            let ok = app.borrow_mut().set_bios("openbios.bin (bundled, MIT licence)", data, true);
                            if ok {
                                toast("OpenBIOS ready — homebrew works; commercial games may not", "ok");
                                maybe_autostart(&app);
                            }
                        }
                        None => toast("OpenBIOS isn't included in this build", "bad"),
                    }
                });
            });
        }
        {
            let app2 = app.clone();
            on_id("demo-btn", "click", move |_| {
                let app = app2.clone();
                spawn_local(async move {
                    match fetch_bytes("test-files/controller.exe").await {
                        Some(data) => {
                            handle_files(&app, vec![("Controller demo".to_string(), data)], false);
                            toast("Controller demo: every button lights up — try the keys, a gamepad or the touch controls", "");
                        }
                        None => toast("The demo isn't included in this build", "bad"),
                    }
                });
            });
        }
        {
            let app2 = app.clone();
            on_id("continue-btn", "click", move |_| {
                let app = app2.clone();
                spawn_local(async move {
                    let Some(db) = app.borrow().db.clone() else { return };
                    toast("Loading saved game…", "");
                    let names = idb::get_string(&db, "last/names").await.unwrap_or_default();
                    let mut files = vec![];
                    for n in names.lines() {
                        match idb::get_bytes(&db, &format!("last/file/{}", n)).await {
                            Some(d) => files.push((n.to_string(), d)),
                            None => return toast("Saved game files are incomplete; please load it again", "bad"),
                        }
                    }
                    app.borrow_mut().settings.remember = false; // already stored
                    handle_files(&app, files, false);
                    let r = input("opt-remember").checked();
                    app.borrow_mut().settings.remember = r;
                });
            });
        }
        {
            let app2 = app.clone();
            on_id("continue-forget", "click", move |_| {
                let app = app2.clone();
                spawn_local(async move {
                    // take the handle first: a borrow held across .await would
                    // collide with the frame loop ("RefCell already borrowed")
                    let db = app.borrow().db.clone();
                    if let Some(db) = db {
                        forget_game(&db).await;
                    }
                    show("continue", false);
                    toast("Forgot the saved game", "");
                });
            });
        }

        // ---- toolbar
        {
            let app = app.clone();
            on_id("btn-pause", "click", move |_| {
                let mut a = app.borrow_mut();
                let p = !a.paused;
                a.set_paused(p);
            });
        }
        {
            let app = app.clone();
            on_id("btn-reset", "click", move |_| {
                let mut a = app.borrow_mut();
                if !a.running {
                    return;
                }
                a.save_cards(true);
                if matches!(a.game, Some((Loaded::Exe(..), _))) {
                    a.boot(true);
                } else {
                    a.psx.reset();
                    a.apply_pad_type();
                    a.set_paused(false);
                    toast("Reset", "");
                }
            });
        }
        {
            let app = app.clone();
            on_id("btn-eject", "click", move |_| app.borrow_mut().eject());
        }
        {
            let app = app.clone();
            on_id("btn-save", "click", move |_| spawn_local(save_state(app.clone())));
        }
        {
            let app = app.clone();
            on_id("btn-load", "click", move |_| spawn_local(load_state(app.clone())));
        }
        {
            let app = app.clone();
            on_id("btn-analog", "click", move |_| {
                let mut a = app.borrow_mut();
                let locked = a.psx.bus.sio.ports[0].pad.locked;
                let on = a.psx.bus.sio.ports[0].pad.toggle_analog();
                toast(if locked { "Analog mode is locked by the game" } else if on { "Analog mode on" } else { "Analog mode off" }, "");
            });
        }
        {
            let app = app.clone();
            on_id("volume", "input", move |_| {
                let mut a = app.borrow_mut();
                a.settings.volume = input("volume").value().parse().unwrap_or(80);
                a.apply_volume();
                a.settings.save();
            });
        }
        on_id("btn-full", "click", move |_| {
            let _ = el("screen-wrap").request_fullscreen();
        });

        // ---- settings dialog
        for id in ["btn-settings", "btn-settings-top"] {
            let app = app.clone();
            on_id(id, "click", move |_| {
                App::render_keymap(&app);
                let _ = el("settings").dyn_into::<HtmlDialogElement>().unwrap().show_modal();
            });
        }
        {
            let app = app.clone();
            on_id("settings-close", "click", move |_| {
                app.borrow_mut().listening = None;
                el("settings").dyn_into::<HtmlDialogElement>().unwrap().close();
            });
        }
        {
            let app = app.clone();
            on_id("opt-smooth", "change", move |_| {
                let mut a = app.borrow_mut();
                a.settings.smooth = input("opt-smooth").checked();
                set_class("screen-wrap", "smooth", a.settings.smooth);
                a.settings.save();
            });
        }
        {
            let app = app.clone();
            on_id("opt-aspect", "change", move |_| {
                let mut a = app.borrow_mut();
                a.settings.stretch = el("opt-aspect").dyn_into::<HtmlSelectElement>().unwrap().value() == "stretch";
                set_class("screen-wrap", "stretch", a.settings.stretch);
                a.settings.save();
            });
        }
        {
            let app = app.clone();
            on_id("opt-pad", "change", move |_| {
                let mut a = app.borrow_mut();
                a.settings.dualshock = el("opt-pad").dyn_into::<HtmlSelectElement>().unwrap().value() == "dualshock";
                a.apply_pad_type();
                a.settings.save();
            });
        }
        {
            let app = app.clone();
            on_id("opt-remember", "change", move |_| {
                let mut a = app.borrow_mut();
                a.settings.remember = input("opt-remember").checked();
                a.settings.save();
                if !a.settings.remember {
                    if let Some(db) = a.db.clone() {
                        spawn_local(async move { forget_game(&db).await });
                    }
                    show("continue", false);
                }
            });
        }
        {
            let app = app.clone();
            on_id("keys-reset", "click", move |_| {
                {
                    let mut a = app.borrow_mut();
                    a.keymap = KeyMap::default();
                    if let Some(st) = storage() {
                        let _ = st.remove_item(K_KEYS);
                    }
                }
                App::render_keymap(&app);
            });
        }
        {
            let app = app.clone();
            on_id("forget-bios", "click", move |_| {
                if let Some(st) = storage() {
                    let _ = st.remove_item(K_BIOS);
                    let _ = st.remove_item(K_BIOS_NAME);
                }
                let mut a = app.borrow_mut();
                a.bios = None;
                set_class("step-bios", "done", false);
                set_text("bios-status", "Your console's BIOS dump, e.g. SCPH1001.BIN (512 KB)");
                a.update_start_buttons();
                toast("BIOS forgotten", "");
            });
        }
        for port in 0..2usize {
            let n = port + 1;
            {
                let app = app.clone();
                on_id(&format!("mc{}-export", n), "click", move |_| {
                    let a = app.borrow();
                    match a.psx.memcard(port) {
                        Some(d) => download(&format!("memcard{}.mcr", n), d),
                        None => toast("No card in that slot", "bad"),
                    }
                });
            }
            {
                let app2 = app.clone();
                let inp = input(&format!("mc{}-import", n));
                on(&inp.clone(), "change", move |_| {
                    let app = app2.clone();
                    let inp = inp.clone();
                    spawn_local(async move {
                        let files = read_list(inp.files()).await;
                        inp.set_value("");
                        let Some((name, data)) = files.into_iter().next() else { return };
                        match logic::import_memcard(&data) {
                            Ok(raw) => {
                                let mut a = app.borrow_mut();
                                a.psx.set_memcard(port, raw);
                                a.save_cards(true);
                                toast(&format!("{} imported into slot {}", name, n), "ok");
                            }
                            Err(e) => toast(&e, "bad"),
                        }
                    });
                });
            }
            {
                let app = app.clone();
                on_id(&format!("mc{}-format", n), "click", move |_| {
                    let ok = window()
                        .confirm_with_message(&format!("Erase everything on memory card {}?", n))
                        .unwrap_or(false);
                    if ok {
                        let mut a = app.borrow_mut();
                        a.psx.set_memcard(port, MemCard::formatted().data);
                        a.save_cards(true);
                        toast(&format!("Memory card {} formatted", n), "");
                    }
                });
            }
        }

        // ---- touch controls (pointer events: multi-touch, mouse and pen)
        {
            let touch_device = window().navigator().max_touch_points() > 0;
            let saved = storage().and_then(|s| s.get_item("psx-touch").ok().flatten());
            let show_touch = match saved.as_deref() {
                Some("1") => true,
                Some("0") => false,
                _ => touch_device,
            };
            let _ = document().body().unwrap().class_list().toggle_with_force("touch-on", show_touch);
            on_id("btn-touch", "click", move |_| {
                let body = document().body().unwrap();
                let shown = body.class_list().toggle("touch-on").unwrap_or(false);
                if let Some(st) = storage() {
                    let _ = st.set_item("psx-touch", if shown { "1" } else { "0" });
                }
            });
            let buttons = document().query_selector_all("#touch [data-btn]").unwrap();
            for k in 0..buttons.length() {
                let node: Element = buttons.item(k).unwrap().dyn_into().unwrap();
                let bit: u16 = node.get_attribute("data-btn").and_then(|v| v.parse().ok()).unwrap_or(0);
                for (ev, down) in [("pointerdown", true), ("pointerup", false), ("pointercancel", false), ("pointerleave", false)] {
                    let app = app.clone();
                    let el2 = node.clone();
                    on(&node, ev, move |e| {
                        e.prevent_default();
                        if down {
                            // keep receiving this finger's events even if it slides off
                            if let Ok(pe) = e.clone().dyn_into::<web_sys::PointerEvent>() {
                                let _ = el2.set_pointer_capture(pe.pointer_id());
                            }
                        }
                        let _ = el2.class_list().toggle_with_force("held", down);
                        if let Ok(mut a) = app.try_borrow_mut() {
                            if down {
                                a.touch |= bit;
                            } else {
                                a.touch &= !bit;
                            }
                        }
                    });
                }
                // no context menu / text selection on long-press
                on(&node, "contextmenu", |e| e.prevent_default());
            }
        }

        // ---- keyboard
        {
            let app = app.clone();
            on(&window(), "keydown", move |e| {
                let e: KeyboardEvent = e.dyn_into().unwrap();
                let code = e.code();
                let listening = app.borrow().listening;
                if let Some(bit) = listening {
                    e.prevent_default();
                    {
                        let mut a = app.borrow_mut();
                        a.listening = None;
                        if code != "Escape" {
                            a.keymap.bind(bit, &code);
                            if let Some(st) = storage() {
                                let _ = st.set_item(K_KEYS, &a.keymap.serialize());
                            }
                        }
                    }
                    App::render_keymap(&app);
                    return;
                }
                if el("settings").dyn_into::<HtmlDialogElement>().unwrap().open() {
                    return;
                }
                let running = app.borrow().running;
                match code.as_str() {
                    "F5" if running => {
                        e.prevent_default();
                        spawn_local(save_state(app.clone()));
                    }
                    "F9" if running => {
                        e.prevent_default();
                        spawn_local(load_state(app.clone()));
                    }
                    "F2" if running => {
                        e.prevent_default();
                        let _ = el("btn-analog").dyn_into::<web_sys::HtmlElement>().map(|b| b.click());
                    }
                    "Escape" if running => {
                        let mut a = app.borrow_mut();
                        let p = !a.paused;
                        a.set_paused(p);
                    }
                    "Tab" if running => {
                        e.prevent_default();
                        app.borrow_mut().fast = true;
                    }
                    _ => {
                        let b = app.borrow().keymap.button_for(&code);
                        if let Some(b) = b {
                            if running {
                                e.prevent_default();
                            }
                            app.borrow_mut().keys |= b;
                        }
                    }
                }
            });
        }
        {
            let app = app.clone();
            on(&window(), "keyup", move |e| {
                let e: KeyboardEvent = e.dyn_into().unwrap();
                let code = e.code();
                let mut a = app.borrow_mut();
                if code == "Tab" {
                    a.fast = false;
                }
                if let Some(b) = a.keymap.button_for(&code) {
                    a.keys &= !b;
                }
            });
        }
        {
            let app = app.clone();
            on(&window(), "blur", move |_| {
                let mut a = app.borrow_mut();
                a.keys = 0;
                a.touch = 0;
                a.fast = false;
            });
        }

        // ---- drag & drop anywhere
        on(&window(), "dragover", move |e| {
            e.prevent_default();
            show("drop", true);
        });
        on(&window(), "dragleave", move |e| {
            let e: DragEvent = e.dyn_into().unwrap();
            if e.client_x() <= 0 || e.client_y() <= 0 || e.related_target().is_none() {
                show("drop", false);
            }
        });
        {
            let app = app.clone();
            on(&window(), "drop", move |e| {
                e.prevent_default();
                show("drop", false);
                let e: DragEvent = e.dyn_into().unwrap();
                let list = e.data_transfer().and_then(|d| d.files());
                let app = app.clone();
                spawn_local(async move {
                    let files = read_list(list).await;
                    if !files.is_empty() {
                        handle_files(&app, files, false);
                    }
                });
            });
        }
        {
            let app = app.clone();
            on(&window(), "beforeunload", move |_| app.borrow_mut().save_cards(true));
        }
        {
            let app = app.clone();
            on(&document(), "visibilitychange", move |_| {
                // keep memory cards safe when the tab is hidden
                if document().hidden() {
                    app.borrow_mut().save_cards(false);
                }
            });
        }

        // ---- frame loop
        let f: Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>> = Rc::new(RefCell::new(None));
        let g = f.clone();
        *g.borrow_mut() = Some(Closure::new(move |t: f64| {
            // never panic if state is momentarily borrowed: skip this frame
            if let Ok(mut a) = app.try_borrow_mut() {
                a.frame(t);
            }
            let cb = f.borrow();
            let _ = window().request_animation_frame(cb.as_ref().unwrap().as_ref().unchecked_ref());
        }));
        let _ = window().request_animation_frame(g.borrow().as_ref().unwrap().as_ref().unchecked_ref());
    }
}
