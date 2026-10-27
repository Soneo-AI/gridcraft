//! JavaScript bridge for embedding GridCraft in another web app on the same page (KoAssist).
//!
//! Unlike `host.rs` (a `postMessage` bridge for a framing page), this one speaks to the page that
//! holds the canvas, as `window.gridcraft`, and exposes the control layer
//! ([`gridcraft_ui_egui::control`]) as it is:
//!
//! ```js
//! gridcraft.open(name, bytes)            // Uint8Array → the app opens it like a dropped file
//! gridcraft.request(method, params)      // Promise<result>: any control method
//!                                        // (engine.execute, document.inspect, sheet.read, …)
//! gridcraft.execute(command, params)     // Promise<result>: shorthand for engine.execute
//! gridcraft.save()                       // Promise<{bytes: Uint8Array, name}>: the workbook, no download
//! gridcraft.onEvent(callback)            // callback({type: "ready" | "dirty" | "saveRequested" | "error", …})
//! gridcraft.ready                        // true once the app runs
//! ```
//!
//! Requests are queued and answered from the UI thread on the next frame ([`Bridge::drain`] runs
//! in `WebShell::logic`), so they never race the app. A request's promise resolves with the
//! control reply's `result`, or rejects with its `error`. `save()` asks the engine for the
//! workbook's bytes (`file.saveBytes`, no download, no dialog) with the name the workbook
//! carries; after each frame ([`Bridge::sync`]) a workbook handed over that way counts as
//! saved, and the page hears when unsaved changes appear or go away.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use gridcraft_ui_egui::control::{self, Outcome};
use gridcraft_ui_egui::{Inbox, SheetApp};
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

/// Most queued requests; beyond that a request is rejected at once.
const MAX_QUEUED: usize = 256;

enum Pending {
    Request {
        method: String,
        params: Value,
        resolve: js_sys::Function,
        reject: js_sys::Function,
    },
    /// `save()`: the download hook hands the bytes to `resolve` instead of the browser.
    Save {
        resolve: js_sys::Function,
        reject: js_sys::Function,
    },
}

#[derive(Clone, Default)]
pub struct Bridge {
    queue: Rc<RefCell<VecDeque<Pending>>>,
    /// Names of the workbooks handed to the page since the last frame.
    saved: Rc<RefCell<Vec<String>>>,
    listeners: Rc<RefCell<Vec<js_sys::Function>>>,
    ctx: Rc<RefCell<Option<egui::Context>>>,
    last_dirty: Rc<Cell<Option<bool>>>,
}

impl Bridge {
    /// The app's context, once it runs: queued requests wake it.
    pub fn attach(&self, ctx: egui::Context) {
        *self.ctx.borrow_mut() = Some(ctx);
    }

    fn wake(&self) {
        if let Some(ctx) = self.ctx.borrow().as_ref() {
            ctx.request_repaint();
        }
    }

    /// Tell the page something happened (`{type, …}`); a listener that throws is dropped.
    pub fn emit(&self, event: Value) {
        let Ok(js) = value_to_js(&event) else { return };
        let listeners = self.listeners.borrow().clone();
        for cb in listeners {
            if cb.call1(&JsValue::NULL, &js).is_err() {
                let failed: &JsValue = cb.as_ref();
                self.listeners.borrow_mut().retain(|l| {
                    let kept: &JsValue = l.as_ref();
                    kept != failed
                });
            }
        }
    }

    /// Hand the workbook's bytes to the page (`{bytes, name}`) and remember it as saved.
    fn hand_over(&self, path: &str, bytes: &[u8], resolve: &js_sys::Function) {
        let name = file_name(path);
        let result = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&result, &"bytes".into(), &js_sys::Uint8Array::from(bytes));
        let _ = js_sys::Reflect::set(&result, &"name".into(), &JsValue::from_str(&name));
        let _ = resolve.call1(&JsValue::NULL, &result);
        self.saved.borrow_mut().push(name);
    }

    /// After each frame: a workbook handed to the page this frame counts as saved under its
    /// name (as after Save on the desktop), and the page hears when unsaved changes appear or
    /// go away.
    pub fn sync(&self, app: &mut SheetApp) {
        let saved = std::mem::take(&mut *self.saved.borrow_mut());
        if let Some(name) = saved.last()
            && let Some(doc) = app.session.active_mut()
        {
            doc.saved = doc.wb.clone();
            if doc.path.is_none() {
                doc.title = name.clone();
            }
        }
        let dirty = app.session.documents().iter().any(|d| d.is_dirty());
        if self.last_dirty.get() != Some(dirty) {
            self.last_dirty.set(Some(dirty));
            self.emit(json!({"type": "dirty", "dirty": dirty}));
        }
    }

    /// Install `window.gridcraft`. `inbox` receives opened files, exactly like dropped ones.
    pub fn install(&self, inbox: Inbox) -> Result<(), String> {
        let window = web_sys::window().ok_or("no window")?;
        let api = js_sys::Object::new();
        let js = |e: JsValue| format!("{e:?}");

        // open(name, bytes)
        let bridge = self.clone();
        let open = Closure::<dyn Fn(JsValue, JsValue)>::new(move |name: JsValue, bytes: JsValue| {
            let name = name.as_string().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Book.xlsx".into());
            let bytes = js_sys::Uint8Array::new(&bytes).to_vec();
            inbox.lock().unwrap_or_else(|e| e.into_inner()).push((name, bytes));
            bridge.wake();
        });
        js_sys::Reflect::set(&api, &"open".into(), open.as_ref().unchecked_ref()).map_err(js)?;
        open.forget();

        // request(method, params) → Promise
        let bridge = self.clone();
        let request = Closure::<dyn Fn(JsValue, JsValue) -> js_sys::Promise>::new(move |method: JsValue, params: JsValue| {
            let bridge = bridge.clone();
            let method = method.as_string().unwrap_or_default();
            let params = js_to_value(&params);
            js_sys::Promise::new(&mut |resolve, reject| {
                bridge.push(Pending::Request { method: method.clone(), params: params.clone(), resolve, reject });
            })
        });
        js_sys::Reflect::set(&api, &"request".into(), request.as_ref().unchecked_ref()).map_err(js)?;
        request.forget();

        // execute(command, params) → Promise (engine.execute)
        let bridge = self.clone();
        let execute = Closure::<dyn Fn(JsValue, JsValue) -> js_sys::Promise>::new(move |command: JsValue, params: JsValue| {
            let bridge = bridge.clone();
            let command = command.as_string().unwrap_or_default();
            let params = js_to_value(&params);
            js_sys::Promise::new(&mut |resolve, reject| {
                bridge.push(Pending::Request {
                    method: "engine.execute".into(),
                    params: json!({"command": command, "params": params}),
                    resolve,
                    reject,
                });
            })
        });
        js_sys::Reflect::set(&api, &"execute".into(), execute.as_ref().unchecked_ref()).map_err(js)?;
        execute.forget();

        // save() → Promise<{bytes, name}>
        let bridge = self.clone();
        let save = Closure::<dyn Fn() -> js_sys::Promise>::new(move || {
            let bridge = bridge.clone();
            js_sys::Promise::new(&mut |resolve, reject| {
                bridge.push(Pending::Save { resolve, reject });
            })
        });
        js_sys::Reflect::set(&api, &"save".into(), save.as_ref().unchecked_ref()).map_err(js)?;
        save.forget();

        // onEvent(callback)
        let listeners = self.listeners.clone();
        let on_event = Closure::<dyn Fn(JsValue)>::new(move |cb: JsValue| {
            if let Ok(f) = cb.dyn_into::<js_sys::Function>() {
                listeners.borrow_mut().push(f);
            }
        });
        js_sys::Reflect::set(&api, &"onEvent".into(), on_event.as_ref().unchecked_ref()).map_err(js)?;
        on_event.forget();

        js_sys::Reflect::set(&api, &"ready".into(), &JsValue::FALSE).map_err(js)?;
        js_sys::Reflect::set(&window, &"gridcraft".into(), &api).map_err(js)?;
        Ok(())
    }

    /// Mark the API ready (the app runs) and tell listeners.
    pub fn set_ready(&self) {
        if let Some(window) = web_sys::window()
            && let Ok(api) = js_sys::Reflect::get(&window, &"gridcraft".into())
        {
            let _ = js_sys::Reflect::set(&api, &"ready".into(), &JsValue::TRUE);
        }
        self.emit(json!({"type": "ready"}));
    }

    fn push(&self, pending: Pending) {
        let mut queue = self.queue.borrow_mut();
        if queue.len() >= MAX_QUEUED {
            drop(queue);
            let reject = match &pending {
                Pending::Request { reject, .. } | Pending::Save { reject, .. } => reject.clone(),
            };
            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str("too many pending requests"));
            return;
        }
        queue.push_back(pending);
        drop(queue);
        self.wake();
    }

    /// Answer every queued request (UI thread, once per frame).
    pub fn drain(&self, app: &mut SheetApp, ctx: &egui::Context) {
        loop {
            let Some(pending) = self.queue.borrow_mut().pop_front() else { break };
            match pending {
                Pending::Request { method, params, resolve, reject } => {
                    // Host-only methods the page may call besides the control layer.
                    if let Some(outcome) = crate::host_options::handle(app, &method, &params) {
                        settle(outcome, &resolve, &reject);
                        continue;
                    }
                    settle(control::handle(app, ctx, &method, &params), &resolve, &reject);
                }
                Pending::Save { resolve, reject } => {
                    // The engine serialises the active workbook; nothing is downloaded and no
                    // dialog opens (on the web a plain `file.save` has no path to save to).
                    let outcome = control::handle(app, ctx, "engine.execute", &json!({"command": "file.saveBytes", "params": {"format": "xlsx"}}));
                    match saved_bytes(outcome) {
                        Ok((name, bytes)) => self.hand_over(&name, &bytes, &resolve),
                        Err(error) => {
                            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str(&error));
                        }
                    }
                }
            }
        }
    }
}

fn settle(outcome: Outcome, resolve: &js_sys::Function, reject: &js_sys::Function) {
    match outcome {
        Outcome::Done(v) => {
            if v.get("ok").and_then(Value::as_bool) == Some(true) {
                let result = v.get("result").cloned().unwrap_or(Value::Null);
                match value_to_js(&result) {
                    Ok(js) => {
                        let _ = resolve.call1(&JsValue::NULL, &js);
                    }
                    Err(e) => {
                        let _ = reject.call1(&JsValue::NULL, &JsValue::from_str(&e));
                    }
                }
            } else {
                let error = v.get("error").and_then(Value::as_str).unwrap_or("request failed");
                let _ = reject.call1(&JsValue::NULL, &JsValue::from_str(error));
            }
        }
        Outcome::Screenshot { .. } => {
            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str("screenshots are not available in the browser"));
        }
    }
}

/// `file.saveBytes`'s reply → the workbook's name and bytes.
fn saved_bytes(outcome: Outcome) -> Result<(String, Vec<u8>), String> {
    let Outcome::Done(v) = outcome else { return Err("unexpected reply".into()) };
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(v.get("error").and_then(Value::as_str).unwrap_or("save failed").to_string());
    }
    let result = v.get("result").cloned().unwrap_or(Value::Null);
    let name = result.get("name").and_then(Value::as_str).unwrap_or("Book.xlsx").to_string();
    let bytes = result
        .get("base64")
        .and_then(Value::as_str)
        .and_then(gridcraft_engine::io::base64_decode)
        .ok_or_else(|| "save produced no bytes".to_string())?;
    Ok((name, bytes))
}

/// The last path component, as the workbook is named when saved.
fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().filter(|n| !n.is_empty()).unwrap_or(path).to_string()
}

/// JS value → JSON (through `JSON.stringify`); `undefined`/`null` → `{}`.
fn js_to_value(v: &JsValue) -> Value {
    if v.is_undefined() || v.is_null() {
        return json!({});
    }
    js_sys::JSON::stringify(v).ok().and_then(|s| s.as_string()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({}))
}

/// JSON → JS value (through `JSON.parse`).
fn value_to_js(v: &Value) -> Result<JsValue, String> {
    let text = serde_json::to_string(v).map_err(|e| e.to_string())?;
    js_sys::JSON::parse(&text).map_err(|e| format!("{e:?}"))
}
