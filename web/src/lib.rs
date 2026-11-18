//! CorrectSound in a web page: the wizard against the simulated room, with the screens and
//! keys of the terminal, drawn on a canvas. Nothing is played or recorded. The page
//! (docs/try/index.html) loads the font and calls [`start`].

#![cfg(target_family = "wasm")]

mod keys;
mod screen;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use correctsound::app::{App, Options};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{HtmlCanvasElement, KeyboardEvent};

use screen::Screen;

/// How often the app looks at the audio and its background work, in milliseconds, as in
/// the terminal.
const TICK_MS: f64 = 50.0;

/// The app and its screen.
struct Page {
    app: App,
    screen: Screen,
    /// When the app next looks at its work (ms on the page's clock).
    next_tick: f64,
    /// A key was handled since the app last looked.
    keyed: bool,
}

/// The wizard as the page runs it: the simulated room, sweeping four times as fast as real
/// time, as `correctsound --simulate` does.
fn wizard() -> App {
    App::new(Options {
        simulate: true,
        ..Options::default()
    })
}

fn window() -> Result<web_sys::Window, JsValue> {
    web_sys::window().ok_or_else(|| JsValue::from_str("no window"))
}

/// Shows `text` in the page's status line (`#status`), for problems.
fn status(text: &str) {
    if let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("status"))
    {
        el.set_text_content(Some(text));
    }
}

/// True when the status line already shows a problem.
fn has_status() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("status"))
        .and_then(|el| el.text_content())
        .is_some_and(|t| !t.is_empty())
}

/// Runs the app on `canvas`, its text in the font `font` (a TrueType file) that the page
/// has loaded as `family`.
///
/// # Errors
///
/// When the font cannot be read or the canvas does not draw.
#[wasm_bindgen]
pub fn start(canvas: HtmlCanvasElement, font: Vec<u8>, family: String) -> Result<(), JsValue> {
    std::panic::set_hook(Box::new(|info| {
        // The first problem is the one to show: what follows is its consequence.
        if !has_status() {
            status(&format!(
                "CorrectSound stopped: {info}. Reload the page to start again."
            ));
        }
        web_sys::console::error_1(&JsValue::from_str(&info.to_string()));
    }));
    // The app's files (sessions, settings) are kept in memory, under a home of their own.
    correctsound::paths::set_root(PathBuf::from("/home/you"));
    let page = Rc::new(RefCell::new(Page {
        app: wizard(),
        screen: Screen::new(canvas, font, family)?,
        next_tick: 0.0,
        keyed: false,
    }));

    let keys = {
        let page = Rc::clone(&page);
        Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
            let Some(key) = keys::convert(&e) else { return };
            // Tab, Space and the arrows are the app's here, not the page's.
            e.prevent_default();
            let mut page = page.borrow_mut();
            page.app.handle_key(key);
            page.keyed = true;
        })
    };
    window()?.add_event_listener_with_callback("keydown", keys.as_ref().unchecked_ref())?;
    keys.forget();

    run(page)
}

/// Draws a frame whenever the browser is ready for one: the app looks at its work every
/// [`TICK_MS`] and after keys, the screen is drawn again when it changed, and the work due
/// in the background (the simulated room's sound, a design) is done right after.
fn run(page: Rc<RefCell<Page>>) -> Result<(), JsValue> {
    let next: Rc<RefCell<Option<Frame>>> = Rc::new(RefCell::new(None));
    let again = Rc::clone(&next);
    let work = Closure::<dyn FnMut()>::new(correctsound::background::work);
    *next.borrow_mut() = Some(Closure::new(move |now: f64| {
        if let Err(e) = frame(&page, now) {
            status(&format!("CorrectSound stopped: {e:?}"));
            return;
        }
        let Ok(window) = window() else { return };
        // After this frame is shown, so a key's answer is not held up by the work.
        let _ = window.set_timeout_with_callback(work.as_ref().unchecked_ref());
        if let Some(f) = again.borrow().as_ref() {
            let _ = window.request_animation_frame(f.as_ref().unchecked_ref());
        }
    }));
    if let Some(f) = next.borrow().as_ref() {
        window()?.request_animation_frame(f.as_ref().unchecked_ref())?;
    }
    Ok(())
}

/// What the browser calls for a frame, with the time.
type Frame = Closure<dyn FnMut(f64)>;

fn frame(page: &Rc<RefCell<Page>>, now: f64) -> Result<(), JsValue> {
    let page = &mut *page.borrow_mut();
    let resized = page.screen.fit();
    let ticked = page.keyed || now >= page.next_tick;
    if ticked {
        page.app.tick();
        page.keyed = false;
        page.next_tick = now + TICK_MS;
    }
    // There is no terminal to go back to: quitting starts again.
    if page.app.quit {
        page.app = wizard();
        page.app.message = Some("Started again from the beginning.".into());
    }
    // Laid out again after the app looked at its work or the canvas changed size; drawn
    // again when that changed what it shows.
    let changed = (ticked || resized) && page.screen.update(&page.app)?;
    if changed || resized {
        page.screen.paint()?;
    }
    Ok(())
}
