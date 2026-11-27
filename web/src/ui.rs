//! Wires the page (static/index.html) to the app: one function per area.

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;

use ps1wasm_core::pad::MemCard;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{DragEvent, Element, HtmlCanvasElement, HtmlDialogElement, HtmlSelectElement, KeyboardEvent};

use crate::app::*;
use crate::dom::*;
use crate::idb;
use crate::logic::{self, KeyMap, Loaded};

type Files = Vec<(String, Vec<u8>)>;

// ---------------------------------------------------------------------- helpers

/// `f(&mut App)` whenever element `id` fires `ev`.
fn on_app(app: &Shared, id: &str, ev: &str, f: impl Fn(&mut App) + 'static) {
    let app = app.clone();
    on_id(id, ev, move |_| f(&mut app.borrow_mut()));
}

/// Start the async action `f(app)` whenever element `id` fires `ev`.
fn on_app_async<Fut: Future<Output = ()> + 'static>(app: &Shared, id: &str, ev: &str, f: impl Fn(Shared) -> Fut + 'static) {
    let app = app.clone();
    on_id(id, ev, move |_| spawn_local(f(app.clone())));
}

/// File picker `id`: read the chosen files, clear the picker, call `f`.
fn on_files(app: &Shared, id: &str, f: impl Fn(&Shared, Files) + 'static) {
    let app = app.clone();
    let f = Rc::new(f);
    let inp = input(id);
    on(&inp.clone(), "change", move |_| {
        let (app, inp, f) = (app.clone(), inp.clone(), f.clone());
        spawn_local(async move {
            let files = read_list(inp.files()).await;
            inp.set_value("");
            f(&app, files);
        });
    });
}

fn select_value(id: &str) -> String {
    el(id).dyn_into::<HtmlSelectElement>().unwrap().value()
}

fn settings_dialog() -> HtmlDialogElement {
    el("settings").dyn_into().unwrap()
}

fn body_class(class: &str, on: Option<bool>) -> bool {
    let list = document().body().unwrap().class_list();
    match on {
        Some(v) => list.toggle_with_force(class, v).unwrap_or(v),
        None => list.toggle(class).unwrap_or(false),
    }
}

// ---------------------------------------------------------------------- entry

#[wasm_bindgen(start)]
pub fn start() {
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(&info.to_string().into());
        toast(&format!("Emulator crashed: {}", info), "bad");
    }));

    let canvas: HtmlCanvasElement = el("screen").dyn_into().unwrap();
    let app: Shared = Rc::new(RefCell::new(App::new(canvas)));

    restore(&app);
    start_card(&app);
    toolbar(&app);
    settings(&app);
    memory_cards(&app);
    touch_controls(&app);
    keyboard(&app);
    window_events(&app);
    frame_loop(app);
}

/// Saved settings into the UI, the remembered BIOS, and the IndexedDB handle.
fn restore(app: &Shared) {
    let s = app.borrow().settings.clone();
    input("opt-smooth").set_checked(s.smooth);
    input("opt-remember").set_checked(s.remember);
    input("volume").set_value(&s.volume.to_string());
    el("opt-aspect").dyn_into::<HtmlSelectElement>().unwrap().set_value(if s.stretch { "stretch" } else { "4:3" });
    el("opt-pad").dyn_into::<HtmlSelectElement>().unwrap().set_value(if s.dualshock { "dualshock" } else { "digital" });
    set_class("screen-wrap", "smooth", s.smooth);
    set_class("screen-wrap", "stretch", s.stretch);
    App::render_help(app);

    if let Some(st) = storage() {
        if let (Ok(Some(b64)), name) = (st.get_item(K_BIOS), st.get_item(K_BIOS_NAME).ok().flatten()) {
            if let Some(data) = logic::b64_decode(&b64) {
                let name = name.unwrap_or_else(|| "BIOS".into());
                app.borrow_mut().set_bios(&format!("{} (remembered)", name), data, false);
            }
        }
    }
    app.borrow_mut().update_start_buttons();

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

fn start_card(app: &Shared) {
    on_files(app, "bios-input", |app, files| handle_files(app, files, true));
    on_id("game-input", "change", |_| set_text("game-status", "Reading files…"));
    on_files(app, "game-input", |app, files| handle_files(app, files, false));
    on_app(app, "start-btn", "click", |a| a.boot(true));
    on_app(app, "bios-only-btn", "click", |a| {
        a.game = None;
        a.boot(false);
    });
    on_app_async(app, "openbios-btn", "click", |app| async move {
        match fetch_bytes("openbios/openbios.bin").await {
            Some(data) => {
                if app.borrow_mut().set_bios("openbios.bin (bundled, MIT licence)", data, true) {
                    toast("OpenBIOS ready — homebrew works; commercial games may not", "ok");
                    maybe_autostart(&app);
                }
            }
            None => toast("OpenBIOS isn't included in this build", "bad"),
        }
    });
    on_app_async(app, "demo-btn", "click", |app| async move {
        match fetch_bytes("test-files/controller.exe").await {
            Some(data) => {
                handle_files(&app, vec![("Controller demo".to_string(), data)], false);
                toast("Controller demo: every button lights up — try the keys, a gamepad or the touch controls", "");
            }
            None => toast("The demo isn't included in this build", "bad"),
        }
    });
    on_app_async(app, "continue-btn", "click", |app| async move {
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
        app.borrow_mut().settings.remember = input("opt-remember").checked();
    });
    on_app_async(app, "continue-forget", "click", |app| async move {
        // take the handle first: a borrow held across .await would collide
        // with the frame loop ("RefCell already borrowed")
        let db = app.borrow().db.clone();
        if let Some(db) = db {
            forget_game(&db).await;
        }
        show("continue", false);
        toast("Forgot the saved game", "");
    });
}

fn toolbar(app: &Shared) {
    on_app(app, "btn-pause", "click", |a| {
        let p = !a.paused;
        a.set_paused(p);
    });
    on_app(app, "btn-reset", "click", |a| {
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
    on_app(app, "btn-eject", "click", |a| a.eject());
    on_app_async(app, "btn-save", "click", save_state);
    on_app_async(app, "btn-load", "click", load_state);
    on_app(app, "btn-analog", "click", |a| {
        let locked = a.psx.bus.sio.ports[0].pad.locked;
        let on = a.psx.bus.sio.ports[0].pad.toggle_analog();
        toast(if locked { "Analog mode is locked by the game" } else if on { "Analog mode on" } else { "Analog mode off" }, "");
    });
    on_app(app, "volume", "input", |a| {
        a.settings.volume = input("volume").value().parse().unwrap_or(80);
        a.apply_volume();
        a.settings.save();
    });
    on_id("btn-full", "click", |_| {
        let _ = el("screen-wrap").request_fullscreen();
    });
    // landscape phones: the toolbar is an overlay behind the ⋯ button, and
    // closes again after picking an action (not when adjusting volume/slot)
    on_id("btn-menu", "click", |_| {
        body_class("menu-open", None);
    });
    on_id("toolbar", "click", |e| {
        let target = e.target().and_then(|t| t.dyn_into::<Element>().ok());
        if target.and_then(|t| t.closest("button").ok().flatten()).is_some() {
            body_class("menu-open", Some(false));
        }
    });
}

fn settings(app: &Shared) {
    for id in ["btn-settings", "btn-settings-top"] {
        let app = app.clone();
        on_id(id, "click", move |_| {
            App::render_keymap(&app);
            let _ = settings_dialog().show_modal();
        });
    }
    on_app(app, "settings-close", "click", |a| {
        a.listening = None;
        settings_dialog().close();
    });
    on_app(app, "opt-smooth", "change", |a| {
        a.settings.smooth = input("opt-smooth").checked();
        set_class("screen-wrap", "smooth", a.settings.smooth);
        a.settings.save();
    });
    on_app(app, "opt-aspect", "change", |a| {
        a.settings.stretch = select_value("opt-aspect") == "stretch";
        set_class("screen-wrap", "stretch", a.settings.stretch);
        a.settings.save();
    });
    on_app(app, "opt-pad", "change", |a| {
        a.settings.dualshock = select_value("opt-pad") == "dualshock";
        a.apply_pad_type();
        a.settings.save();
    });
    on_app(app, "opt-remember", "change", |a| {
        a.settings.remember = input("opt-remember").checked();
        a.settings.save();
        if !a.settings.remember {
            if let Some(db) = a.db.clone() {
                spawn_local(async move { forget_game(&db).await });
            }
            show("continue", false);
        }
    });
    {
        let app = app.clone();
        on_id("keys-reset", "click", move |_| {
            app.borrow_mut().keymap = KeyMap::default();
            if let Some(st) = storage() {
                let _ = st.remove_item(K_KEYS);
            }
            App::render_keymap(&app);
        });
    }
    on_app(app, "forget-bios", "click", |a| {
        if let Some(st) = storage() {
            let _ = st.remove_item(K_BIOS);
            let _ = st.remove_item(K_BIOS_NAME);
        }
        a.bios = None;
        set_class("step-bios", "done", false);
        set_text("bios-status", "Your console's BIOS dump, e.g. SCPH1001.BIN (512 KB)");
        a.update_start_buttons();
        toast("BIOS forgotten", "");
    });
}

/// Export / import / format buttons for both memory card slots.
fn memory_cards(app: &Shared) {
    for port in 0..2usize {
        let n = port + 1;
        on_app(app, &format!("mc{}-export", n), "click", move |a| match a.psx.memcard(port) {
            Some(d) => download(&format!("memcard{}.mcr", n), d),
            None => toast("No card in that slot", "bad"),
        });
        on_files(app, &format!("mc{}-import", n), move |app, files| {
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
        on_app(app, &format!("mc{}-format", n), "click", move |a| {
            let ok = window().confirm_with_message(&format!("Erase everything on memory card {}?", n)).unwrap_or(false);
            if ok {
                a.psx.set_memcard(port, MemCard::formatted().data);
                a.save_cards(true);
                toast(&format!("Memory card {} formatted", n), "");
            }
        });
    }
}

/// On-screen buttons (pointer events: multi-touch, mouse and pen). Shown by
/// default on touch devices; the 🎮 button toggles and remembers the choice.
fn touch_controls(app: &Shared) {
    let shown = match storage().and_then(|s| s.get_item("psx-touch").ok().flatten()).as_deref() {
        Some("1") => true,
        Some("0") => false,
        _ => window().navigator().max_touch_points() > 0,
    };
    body_class("touch-on", Some(shown));
    on_id("btn-touch", "click", |_| {
        let shown = body_class("touch-on", None);
        if let Some(st) = storage() {
            let _ = st.set_item("psx-touch", if shown { "1" } else { "0" });
        }
    });

    let buttons = document().query_selector_all("#touch [data-btn]").unwrap();
    for k in 0..buttons.length() {
        let node: Element = buttons.item(k).unwrap().dyn_into().unwrap();
        let bit: u16 = node.get_attribute("data-btn").and_then(|v| v.parse().ok()).unwrap_or(0);
        for (ev, down) in [("pointerdown", true), ("pointerup", false), ("pointercancel", false), ("pointerleave", false)] {
            let (app, button) = (app.clone(), node.clone());
            on(&node, ev, move |e| {
                e.prevent_default();
                if down {
                    // keep receiving this finger's events even if it slides off
                    if let Ok(pe) = e.dyn_into::<web_sys::PointerEvent>() {
                        let _ = button.set_pointer_capture(pe.pointer_id());
                    }
                }
                let _ = button.class_list().toggle_with_force("held", down);
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

/// Pad keys, hotkeys (F5/F9 save/load, F2 analog, Esc pause, Tab fast
/// forward) and key rebinding in the settings dialog.
fn keyboard(app: &Shared) {
    let app2 = app.clone();
    on(&window(), "keydown", move |e| {
        let app = &app2;
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
            App::render_keymap(app);
            return;
        }
        if settings_dialog().open() {
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
    let app2 = app.clone();
    on(&window(), "keyup", move |e| {
        let code = e.dyn_into::<KeyboardEvent>().unwrap().code();
        let mut a = app2.borrow_mut();
        if code == "Tab" {
            a.fast = false;
        }
        if let Some(b) = a.keymap.button_for(&code) {
            a.keys &= !b;
        }
    });
}

/// Drag & drop anywhere, releasing input on blur, and saving memory cards
/// when the tab is hidden or closed.
fn window_events(app: &Shared) {
    let w = window();
    on(&w, "dragover", |e| {
        e.prevent_default();
        show("drop", true);
    });
    on(&w, "dragleave", |e| {
        let e: DragEvent = e.dyn_into().unwrap();
        if e.client_x() <= 0 || e.client_y() <= 0 || e.related_target().is_none() {
            show("drop", false);
        }
    });
    let app2 = app.clone();
    on(&w, "drop", move |e| {
        e.prevent_default();
        show("drop", false);
        let list = e.dyn_into::<DragEvent>().unwrap().data_transfer().and_then(|d| d.files());
        let app = app2.clone();
        spawn_local(async move {
            let files = read_list(list).await;
            if !files.is_empty() {
                handle_files(&app, files, false);
            }
        });
    });
    let app2 = app.clone();
    on(&w, "blur", move |_| {
        let mut a = app2.borrow_mut();
        a.keys = 0;
        a.touch = 0;
        a.fast = false;
    });
    let app2 = app.clone();
    on(&w, "beforeunload", move |_| app2.borrow_mut().save_cards(true));
    let app2 = app.clone();
    on(&document(), "visibilitychange", move |_| {
        if document().hidden() {
            app2.borrow_mut().save_cards(false);
        }
    });
}

/// requestAnimationFrame loop driving the emulator.
fn frame_loop(app: Shared) {
    type FrameCb = Closure<dyn FnMut(f64)>;
    let f: Rc<RefCell<Option<FrameCb>>> = Rc::new(RefCell::new(None));
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
