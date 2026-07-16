//! AGENT SEAM: dev-only agent instrumentation for the iced event loop.
//!
//! Three seams: (1) an AccessKit adapter attached to every window at creation,
//! (2) [`set_tree`] pushing semantic tree updates into that adapter, and
//! (3) [`inject`] feeding synthetic iced-core events into the same runtime
//! path real input takes. Cross-thread bridge state is process-global, while
//! platform adapters stay on the event-loop thread that created them.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};

use crate::core::window::Id;

/// Per-window adapter, confined to the winit event-loop thread. In particular,
/// macOS AccessKit adapters own AppKit state and are intentionally `!Send`.
struct Slot {
    adapter: accesskit_winit::Adapter,
}

#[derive(Default)]
struct Local {
    slots: HashMap<Id, Slot>,
    winit_to_iced: HashMap<winit::window::WindowId, Id>,
}

struct Globals {
    /// Weak windows remain cross-thread so an injected event can wake redraw
    /// without extending any native window's lifetime.
    windows: Mutex<HashMap<Id, std::sync::Weak<winit::window::Window>>>,
    /// Last trees are shared with both AccessKit activation and bridge reads.
    last_trees: Mutex<HashMap<Id, Arc<Mutex<Option<accesskit::TreeUpdate>>>>>,
    injected: Mutex<Vec<(Id, crate::core::Event)>>,
    injected_flag: AtomicBool,
    actions_tx: Sender<(Id, accesskit::ActionRequest)>,
    actions_rx: Mutex<Option<Receiver<(Id, accesskit::ActionRequest)>>>,
}

thread_local! {
    static LOCAL: RefCell<Local> = RefCell::new(Local::default());
}

fn globals() -> &'static Globals {
    static G: OnceLock<Globals> = OnceLock::new();
    G.get_or_init(|| {
        let (tx, rx) = channel();
        Globals {
            windows: Mutex::new(HashMap::new()),
            last_trees: Mutex::new(HashMap::new()),
            injected: Mutex::new(Vec::new()),
            injected_flag: AtomicBool::new(false),
            actions_tx: tx,
            actions_rx: Mutex::new(Some(rx)),
        }
    })
}

/// The platform adapters' async plumbing (zbus on Linux) may require an
/// ambient Tokio reactor; the winit event-loop thread has none, so the agent
/// owns a tiny one and enters it around every adapter interaction.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("iced-agent-a11y")
            .enable_all()
            .build()
            .expect("build agent tokio runtime")
    })
}

/// Replays the last pushed tree when the platform activates accessibility.
struct Activation {
    last_tree: Arc<Mutex<Option<accesskit::TreeUpdate>>>,
}

impl accesskit::ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<accesskit::TreeUpdate> {
        self.last_tree.lock().unwrap().clone()
    }
}

struct Actions {
    id: Id,
    tx: Sender<(Id, accesskit::ActionRequest)>,
}

impl accesskit::ActionHandler for Actions {
    fn do_action(&mut self, request: accesskit::ActionRequest) {
        let _ = self.tx.send((self.id, request));
    }
}

struct Deactivation;

impl accesskit::DeactivationHandler for Deactivation {
    fn deactivate_accessibility(&mut self) {}
}

/// Seam 1: called by the event loop right after `create_window`, while the
/// window is still invisible (the adapter requires pre-visibility creation).
pub(crate) fn attach(
    event_loop: &winit::event_loop::ActiveEventLoop,
    id: Id,
    window: &Arc<winit::window::Window>,
) {
    let g = globals();
    log::info!("agent: attaching AccessKit adapter to window {id:?}");
    let last_tree = Arc::new(Mutex::new(None));
    let _reactor = runtime().enter();
    let adapter = accesskit_winit::Adapter::with_direct_handlers(
        event_loop,
        window,
        Activation {
            last_tree: Arc::clone(&last_tree),
        },
        Actions {
            id,
            tx: g.actions_tx.clone(),
        },
        Deactivation,
    );
    LOCAL.with(|local| {
        let mut local = local.borrow_mut();
        let _ = local.winit_to_iced.insert(window.id(), id);
        let _ = local.slots.insert(id, Slot { adapter });
    });
    let _ = g.windows.lock().unwrap().insert(id, Arc::downgrade(window));
    let _ = g.last_trees.lock().unwrap().insert(id, last_tree);
}

/// Seam 1: forward every winit window event to the window's adapter.
pub(crate) fn process_event(winit_id: winit::window::WindowId, event: &winit::event::WindowEvent) {
    let g = globals();
    LOCAL.with(|local| {
        let mut local = local.borrow_mut();
        let Some(id) = local.winit_to_iced.get(&winit_id).copied() else {
            return;
        };
        let window = g
            .windows
            .lock()
            .unwrap()
            .get(&id)
            .and_then(std::sync::Weak::upgrade);
        if let (Some(slot), Some(window)) = (local.slots.get_mut(&id), window) {
            let _reactor = runtime().enter();
            slot.adapter.process_event(&window, event);
        }
        if matches!(event, winit::event::WindowEvent::Destroyed) {
            let _ = local.slots.remove(&id);
            let _ = local.winit_to_iced.remove(&winit_id);
            let _ = g.windows.lock().unwrap().remove(&id);
            let _ = g.last_trees.lock().unwrap().remove(&id);
        }
    });
}

/// Seam 2: push a semantic tree for a window (app-side plugin calls this).
pub fn set_tree(id: Id, update: accesskit::TreeUpdate) {
    let g = globals();
    let last_tree = g.last_trees.lock().unwrap().get(&id).cloned();
    if let Some(last_tree) = last_tree {
        *last_tree.lock().unwrap() = Some(update.clone());
    }
    LOCAL.with(|local| {
        if let Some(slot) = local.borrow_mut().slots.get_mut(&id) {
            let _reactor = runtime().enter();
            slot.adapter.update_if_active(|| update);
        }
    });
}

/// Seam 2: the plugin takes the (single) ActionRequest receiver at boot.
pub fn take_action_rx() -> Option<Receiver<(Id, accesskit::ActionRequest)>> {
    globals().actions_rx.lock().unwrap().take()
}

/// Dump the last tree pushed for a window (the `iced_a11y` tool).
pub fn last_tree(id: Id) -> Option<accesskit::TreeUpdate> {
    globals()
        .last_trees
        .lock()
        .unwrap()
        .get(&id)
        .and_then(|tree| tree.lock().unwrap().clone())
}

/// Seam 3: queue a synthetic iced-core event; the loop drains it into the
/// same per-window event vector real input lands in, then redraws.
pub fn inject(id: Id, event: crate::core::Event) {
    let g = globals();
    g.injected.lock().unwrap().push((id, event));
    g.injected_flag.store(true, Ordering::Release);
    let window = g
        .windows
        .lock()
        .unwrap()
        .get(&id)
        .and_then(std::sync::Weak::upgrade);
    if let Some(window) = window {
        window.request_redraw();
    }
}

/// Seam 3: drained by the event loop each cycle.
pub(crate) fn drain_injected() -> Vec<(Id, crate::core::Event)> {
    let g = globals();
    if !g.injected_flag.swap(false, Ordering::AcqRel) {
        return Vec::new();
    }
    std::mem::take(&mut *g.injected.lock().unwrap())
}

/// Windows currently alive (the `iced_windows` tool).
pub fn window_ids() -> Vec<Id> {
    globals().windows.lock().unwrap().keys().copied().collect()
}
