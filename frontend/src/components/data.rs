//! Data-loading hook shared by every page.
use std::cell::Cell;
use std::rc::Rc;

use serde_json::Value;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api::{self, ApiError};
use crate::components::ui::{Alert, Spinner};

#[derive(Clone, PartialEq)]
pub enum Fetch {
    Loading,
    Ready(Value),
    Failed(ApiError),
}

/// GET `path` whenever `path` or `tick` changes. Bump `tick` after a mutation to reload.
/// Responses that arrive after the path changed again are dropped.
#[hook]
pub fn use_fetch(path: String, tick: u32) -> Fetch {
    let state = use_state(|| Fetch::Loading);
    let generation = use_mut_ref(|| Rc::new(Cell::new(0u32)));
    {
        let state = state.clone();
        use_effect_with((path, tick), move |(p, _)| {
            let gen = generation.borrow().clone();
            gen.set(gen.get() + 1);
            let mine = gen.get();
            let p = p.clone();
            state.set(Fetch::Loading);
            spawn_local(async move {
                let r = api::get(&p).await;
                if gen.get() == mine {
                    state.set(match r {
                        Ok(v) => Fetch::Ready(v),
                        Err(e) => Fetch::Failed(e),
                    });
                }
            });
            || ()
        });
    }
    (*state).clone()
}

/// Render the three states of a fetch.
pub fn show(f: &Fetch, ok: impl FnOnce(&Value) -> Html) -> Html {
    match f {
        Fetch::Loading => html! { <Spinner /> },
        Fetch::Failed(e) => html! { <Alert tone="err">{ e.message.clone() }</Alert> },
        Fetch::Ready(v) => ok(v),
    }
}
