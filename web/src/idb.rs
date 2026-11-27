//! Minimal async IndexedDB key/value store (one object store, "kv").
//! Values are byte arrays (save states, game files) or strings (metadata).

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{IdbDatabase, IdbRequest, IdbTransactionMode};

const DB: &str = "psx-emulator";
const STORE: &str = "kv";

/// Turn an IdbRequest into a future resolving to its result.
async fn wait(req: &IdbRequest) -> Result<JsValue, JsValue> {
    let req2 = req.clone();
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let r = req2.clone();
        let ok = Closure::once_into_js(move |_e: web_sys::Event| {
            let v = r.result().unwrap_or(JsValue::UNDEFINED);
            let _ = resolve.call1(&JsValue::NULL, &v);
        });
        let r = req2.clone();
        let err = Closure::once_into_js(move |_e: web_sys::Event| {
            let e = r.error().ok().flatten().map(JsValue::from).unwrap_or(JsValue::from_str("IndexedDB error"));
            let _ = reject.call1(&JsValue::NULL, &e);
        });
        req2.set_onsuccess(Some(ok.unchecked_ref()));
        req2.set_onerror(Some(err.unchecked_ref()));
    });
    JsFuture::from(promise).await
}

pub async fn open() -> Result<IdbDatabase, JsValue> {
    let factory = web_sys::window().unwrap().indexed_db()?.ok_or("IndexedDB unavailable")?;
    let req = factory.open_with_u32(DB, 1)?;
    let upgrade = Closure::once_into_js(move |e: web_sys::Event| {
        let req: web_sys::IdbOpenDbRequest = e.target().unwrap().unchecked_into();
        let db: IdbDatabase = req.result().unwrap().unchecked_into();
        if !db.object_store_names().contains(STORE) {
            let _ = db.create_object_store(STORE);
        }
    });
    req.set_onupgradeneeded(Some(upgrade.unchecked_ref()));
    Ok(wait(&req).await?.unchecked_into())
}

pub async fn put(db: &IdbDatabase, key: &str, value: &JsValue) -> Result<(), JsValue> {
    let tx = db.transaction_with_str_and_mode(STORE, IdbTransactionMode::Readwrite)?;
    let req = tx.object_store(STORE)?.put_with_key(value, &JsValue::from_str(key))?;
    wait(&req).await.map(|_| ())
}

pub async fn put_bytes(db: &IdbDatabase, key: &str, data: &[u8]) -> Result<(), JsValue> {
    put(db, key, &js_sys::Uint8Array::from(data).into()).await
}

pub async fn get(db: &IdbDatabase, key: &str) -> Result<JsValue, JsValue> {
    let tx = db.transaction_with_str(STORE)?;
    let req = tx.object_store(STORE)?.get(&JsValue::from_str(key))?;
    wait(&req).await
}

pub async fn get_bytes(db: &IdbDatabase, key: &str) -> Option<Vec<u8>> {
    let v = get(db, key).await.ok()?;
    if v.is_undefined() || v.is_null() {
        return None;
    }
    Some(js_sys::Uint8Array::new(&v).to_vec())
}

pub async fn get_string(db: &IdbDatabase, key: &str) -> Option<String> {
    get(db, key).await.ok()?.as_string()
}

pub async fn delete(db: &IdbDatabase, key: &str) -> Result<(), JsValue> {
    let tx = db.transaction_with_str_and_mode(STORE, IdbTransactionMode::Readwrite)?;
    let req = tx.object_store(STORE)?.delete(&JsValue::from_str(key))?;
    wait(&req).await.map(|_| ())
}
