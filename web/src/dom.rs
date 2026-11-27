//! Small browser helpers: element lookup, events, toasts, files, downloads.

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Document, Element, Event, FileList, HtmlButtonElement, HtmlInputElement, Storage, Window};

pub(crate) fn window() -> Window {
    web_sys::window().expect("no window")
}
pub(crate) fn document() -> Document {
    window().document().expect("no document")
}
pub(crate) fn storage() -> Option<Storage> {
    window().local_storage().ok().flatten()
}
pub(crate) fn el(id: &str) -> Element {
    document().get_element_by_id(id).unwrap_or_else(|| panic!("#{} missing", id))
}
pub(crate) fn input(id: &str) -> HtmlInputElement {
    el(id).dyn_into().unwrap()
}
pub(crate) fn set_text(id: &str, t: &str) {
    el(id).set_text_content(Some(t));
}
pub(crate) fn show(id: &str, on: bool) {
    let _ = el(id).class_list().toggle_with_force("hidden", !on);
}
pub(crate) fn set_class(id: &str, class: &str, on: bool) {
    let _ = el(id).class_list().toggle_with_force(class, on);
}
pub(crate) fn set_disabled(id: &str, d: bool) {
    if let Ok(b) = el(id).dyn_into::<HtmlButtonElement>() {
        b.set_disabled(d);
    }
}
pub(crate) fn on<F: FnMut(Event) + 'static>(target: &web_sys::EventTarget, ev: &str, f: F) {
    let c = Closure::<dyn FnMut(Event)>::new(f);
    target.add_event_listener_with_callback(ev, c.as_ref().unchecked_ref()).unwrap();
    c.forget();
}
pub(crate) fn on_id<F: FnMut(Event) + 'static>(id: &str, ev: &str, f: F) {
    on(&el(id), ev, f);
}
pub(crate) fn now() -> f64 {
    window().performance().map(|p| p.now()).unwrap_or(0.0)
}

pub(crate) fn toast(msg: &str, kind: &str) {
    let t = document().create_element("div").unwrap();
    t.set_class_name(&format!("toast {}", kind));
    t.set_text_content(Some(msg));
    el("toasts").append_child(&t).unwrap();
    let t2 = t.clone();
    let cb = Closure::once_into_js(move || t2.remove());
    let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), 3200);
}

pub(crate) fn download(name: &str, data: &[u8]) {
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
pub(crate) async fn fetch_bytes(url: &str) -> Option<Vec<u8>> {
    let resp: web_sys::Response = JsFuture::from(window().fetch_with_str(url)).await.ok()?.dyn_into().ok()?;
    if !resp.ok() {
        return None;
    }
    let buf = JsFuture::from(resp.array_buffer().ok()?).await.ok()?;
    Some(js_sys::Uint8Array::new(&buf).to_vec())
}

pub(crate) async fn read_list(list: Option<FileList>) -> Vec<(String, Vec<u8>)> {
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
