//! Owns the app state. Background threads never touch it: they send a [`Msg`]
//! to the main thread, and [`Controller::render`] brings the views in line.

use super::actions::{Action, Actions};
use super::colima_page::{ColimaPage, ProfilePick};
use super::hud::{EmptyState, HUD_CLEARANCE, Hud};
use super::logs::{LogCleaner, LogPanel};
use super::pages::{self, Page};
use super::sidebar::{Sidebar, StatusView};
use super::table::{Table, filter_rows};
use super::toolbar::Toolbar;
use super::util::*;
use crate::backend::colima::{
    Colima, ColimaInfo, DEFAULT_PROFILE, ResourceChange, Resources, VM_KINDS, VmUsage, config_file, disk_on_mac,
    find_tool, profile_choices, profile_name_problem,
};
use crate::backend::docker::{
    ContainerAction, DResult, Docker, RunForm, Scope, Snapshot, compose, compose_up, compose_up_args, exec_script,
    run_args, run_image, shared_with_vm, unshared_mounts,
};
use crate::backend::http::Cancel;
use crate::backend::model::*;
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchTime};
use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2::sel;
use objc2_app_kit::*;
use objc2_foundation::*;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The profile in use, kept across launches.
const PROFILE_KEY: &str = "DPColimaProfile";
const LOG_TAIL: u32 = 1000;
const LOG_PANEL_HEIGHT: f64 = 250.0;
/// Text reaching AppKit is capped at one buffer per flush, about 5 MB/s. That is
/// far beyond reading speed, and it keeps a runaway container from costing a
/// whole core.
const LOG_BUFFER_LIMIT: usize = 512 * 1024;
const LOG_FLUSH: Duration = Duration::from_millis(100);
const LOG_BACKOFF: Duration = Duration::from_millis(15);
/// How long the reader may wait on the text view before following is a lie: past
/// this it gives up on the backlog and reconnects at the end of the log.
const LOG_MAX_BEHIND: Duration = Duration::from_secs(2);
const LOG_JUMP_TAIL: u32 = 200;
const LOG_JUMP_NOTE: &str = "\n--- too much output to show it all: skipped ahead to the latest lines ---\n\n";
/// Coalesces bursts of engine events, such as a `compose up`, into one refresh.
const REFRESH_DEBOUNCE: Duration = Duration::from_millis(120);
/// How soon to fetch a list again after the engine failed to give it.
const REFRESH_RETRY: Duration = Duration::from_secs(2);
/// Keeps "Up 3 minutes" and relative dates from going stale without any events.
const TICK: Duration = Duration::from_secs(30);
const ENGINE_RETRY: Duration = Duration::from_secs(2);
/// How often to sample, and only while a sampler is running at all. Each VM
/// reading starts a chain of processes through `colima ssh`.
const STATS_PERIOD: Duration = Duration::from_secs(2);
const VM_MEMORY_PERIOD: Duration = Duration::from_secs(30);
/// How long a reading of the VM's disk stands when no sampler is running. It is
/// taken when the app comes to the front, never on a timer.
const VM_DISK_PERIOD: Duration = Duration::from_secs(600);
/// Between stats passes the sampler rests this many times as long as the pass
/// took, so measuring stays a small share of the engine's time at any scale.
const STATS_REST_RATIO: u32 = 20;
/// How quickly a resting sampler notices it should stop or measure something else.
const SAMPLER_WAKE: Duration = Duration::from_millis(250);
/// How often to re-ask Colima while it says "running" but Docker is silent.
const COLIMA_RECHECK: Duration = Duration::from_secs(3);
/// A VM can stay running with no Docker for good: a containerd profile, or a
/// crashed dockerd. Each check starts three processes, so they thin out to this.
const COLIMA_RECHECK_LONGEST: Duration = Duration::from_secs(60);
/// The pause before reopening an event stream that ended, doubling while they
/// keep ending at once, up to the ceiling.
const EVENTS_RETRY: Duration = Duration::from_millis(300);
const EVENTS_RETRY_MAX: Duration = Duration::from_secs(30);
/// A stream that ran this long was healthy, so its end starts the pauses over.
const EVENTS_HEALTHY: Duration = Duration::from_secs(10);

pub enum Msg {
    /// Numbered in the order they were asked for, and tagged with the profile, so
    /// a late reply cannot overwrite a newer one or land on another profile. No
    /// info means Colima gave no answer in time.
    Colima(u64, String, Option<Box<ColimaInfo>>),
    /// The watcher's generation, and whether the engine answered.
    Engine(u64, bool),
    Changed(Scope),
    // Engine replies carry the epoch they were asked in, see `State::epoch`.
    Snapshot(u64, Snapshot),
    Version(u64, Option<EngineVersion>),
    VolumeSizes(u64, HashMap<String, i64>),
    CliLine(String),
    CliDone(Result<(), String>),
    ActionDone(Option<String>),
    PullProgress(String),
    PullDone(Result<(), String>),
    LogEnded(u64, Option<String>),
    Details(DResult<Box<ContainerDetails>>),
    Storage(u64, DResult<Vec<StorageLine>>, Option<u64>),
    Pruned(DResult<i64>),
    Stats(StatsReading),
    VmUsage(u64, VmUsage),
    PrunePreview(u64, Prune, DResult<PrunePreview>),
}

thread_local! {
    static CTL: OnceCell<Rc<Controller>> = const { OnceCell::new() };
}

/// How long to wait before asking Colima again, after `tries` unsettled checks.
fn recheck_delay(tries: u32) -> Duration {
    (COLIMA_RECHECK * 2u32.saturating_pow(tries)).min(COLIMA_RECHECK_LONGEST)
}

/// Runs `body` with the controller. Called from AppKit callbacks on the main
/// thread; does nothing before launch has finished.
pub fn with_ctl<R>(body: impl FnOnce(&Controller) -> R) -> Option<R> {
    let ctl = CTL.with(|cell| cell.get().cloned())?;
    Some(body(&ctl))
}

/// Delivers a message from any thread to the main thread.
pub fn post(msg: Msg) {
    DispatchQueue::main().exec_async(move || {
        with_ctl(|ctl| ctl.handle(msg));
    });
}

fn after_main(delay: Duration, body: impl FnOnce() + Send + 'static) {
    let when = DispatchTime::try_from(delay).expect("short delay");
    DispatchQueue::main().after(when, body).expect("queue accepts work");
}

/// Waits out `period`, unless `done` says there is no need to.
fn rest(period: Duration, done: impl Fn() -> bool) {
    let end = Instant::now() + period;
    while Instant::now() < end && !done() {
        thread::sleep(SAMPLER_WAKE);
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum ColimaOp {
    Start,
    Stop,
    Restart,
    /// Starts with a new size, stopping first when the VM is up.
    Resize { change: ResourceChange, restart: bool },
    Delete,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Phase {
    Unknown,
    NotInstalled,
    Stopped,
    Starting,
    Stopping,
    /// The VM is up but the Docker socket does not answer (yet).
    NoEngine,
    Running,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum SampleKind {
    /// CPU and memory of the selected containers.
    Containers,
    /// Memory in use inside the VM.
    Vm,
}

/// At most one of these runs, and only while its numbers are on screen.
struct Sampler {
    kind: SampleKind,
    stop: Arc<AtomicBool>,
    targets: Arc<Mutex<Vec<String>>>,
}

struct LogSession {
    generation: u64,
    container: String,
    cancel: Cancel,
}

#[derive(Default)]
pub struct State {
    pub page: Page,
    navigated: bool,
    /// The Colima profile everything on screen belongs to.
    pub profile: String,
    /// Whether the saved profile has been checked against `colima list`.
    profile_checked: bool,
    pub info: Option<ColimaInfo>,
    pub engine_up: bool,
    engine: Option<EngineVersion>,
    op: Option<ColimaOp>,
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub volumes: Vec<Volume>,
    pub networks: Vec<Network>,
    sizes: HashMap<String, i64>,
    filter: String,
    refreshing: bool,
    /// Lists waiting to be fetched by the next refresh.
    stale: Scope,
    refresh_scheduled: bool,
    /// The last refresh failed, so a list on screen may be out of date.
    retrying: bool,
    /// Counts the engine coming and going. A reply asked for in an earlier epoch
    /// describes an engine that has since gone or been swapped, and is dropped.
    epoch: u64,
    /// `/system/df` walks the disk: one at a time, and at most one more queued.
    storage_loading: bool,
    storage_again: bool,
    storage_error: Option<String>,
    /// The Colima recheck waiting to fire, by number, so one that is no longer
    /// wanted can be told apart from the one that is.
    recheck_timer: Option<u64>,
    recheck_timers: u64,
    /// Checks in a row that left Colima unsettled.
    recheck_tries: u32,
    /// The number of the latest Colima reply asked for, and shown.
    colima_asked: u64,
    colima_shown: u64,
    /// Colima's latest reply never came, so `info` may be out of date.
    colima_silent: bool,
    busy_actions: usize,
    pull: Option<String>,
    logs: Option<LogSession>,
    log_generation: u64,
    pub running_only: bool,
    /// Compose projects folded shut.
    collapsed: HashSet<String>,
    pub storage: Vec<StorageLine>,
    mac_disk: Option<u64>,
    pub stats_on: bool,
    sampler: Option<Sampler>,
    vm_usage: VmUsage,
    vm_disk_read: Option<Instant>,
}

impl State {
    /// Colima settled, so the next doubt starts from the shortest wait. A long
    /// wait left running would hold that first check back by up to a minute.
    fn forget_rechecks(&mut self) {
        self.recheck_tries = 0;
        self.recheck_timer = None;
    }
}

pub struct Controller {
    pub mtm: MainThreadMarker,
    pub window: Retained<NSWindow>,
    /// Controls hold their target weakly, so this keeps it alive.
    actions: Retained<Actions>,
    pub sidebar: Sidebar,
    pub toolbar: Toolbar,
    pub containers: Table,
    pub images: Table,
    pub volumes: Table,
    pub networks: Table,
    pub storage: Table,
    pub colima_page: ColimaPage,
    pub logs: LogPanel,
    pub hud: Hud,
    pub empty: EmptyState,
    detail: Retained<NSSplitView>,
    _split: Retained<NSSplitViewController>,
    colima: Option<Colima>,
    docker: Arc<Mutex<Docker>>,
    pub st: RefCell<State>,
    #[cfg(feature = "devtools")]
    pub page_log: RefCell<Vec<String>>,
}

/// Builds the window and everything in it, then starts watching the engine.
pub fn launch(mtm: MainThreadMarker, actions: Retained<Actions>) {
    use super::table::Column;

    let table = |columns: &'static [Column], menu: &[Option<(&str, Action)>]| Table::new(mtm, columns, &actions, menu);
    let containers = table(pages::CONTAINER_COLUMNS, &[
        Some(("Start", Action::ContainerStart)),
        Some(("Stop", Action::ContainerStop)),
        Some(("Restart", Action::ContainerRestart)),
        Some(("Pause", Action::ContainerPause)),
        Some(("Resume", Action::ContainerResume)),
        None,
        Some(("Show Logs", Action::ShowLogs)),
        Some(("Show Details", Action::ShowDetails)),
        Some(("Open in Terminal", Action::OpenTerminal)),
        Some(("Open in Browser", Action::OpenPort)),
        Some(("Show in Finder", Action::RevealMount)),
        None,
        Some(("Select Project", Action::SelectProject)),
        Some(("Copy ID", Action::CopyId)),
        None,
        Some(("Remove…", Action::Remove)),
    ]);
    let images = table(pages::IMAGE_COLUMNS, &[
        Some(("Run…", Action::RunImage)),
        None,
        Some(("Copy ID", Action::CopyId)),
        None,
        Some(("Remove…", Action::Remove)),
    ]);
    let volumes = table(pages::VOLUME_COLUMNS, &[Some(("Copy Name", Action::CopyId)), None, Some(("Remove…", Action::Remove))]);
    let networks = table(pages::NETWORK_COLUMNS, &[
        Some(("Copy ID", Action::CopyId)),
        None,
        Some(("Remove…", Action::Remove)),
        Some(("Remove Unused…", Action::Prune)),
    ]);
    let storage = table(pages::STORAGE_COLUMNS, &[Some(("Remove Unused…", Action::Prune))]);

    let colima_page = ColimaPage::new(mtm, &actions);
    let hud = Hud::new(mtm);
    let empty = EmptyState::new(mtm, &actions);
    let logs = LogPanel::new(mtm, &actions);

    let pages_view = NSView::new(mtm);
    for view in [&*containers.scroll as &NSView, &images.scroll, &volumes.scroll, &networks.scroll, &storage.scroll, &colima_page.view] {
        pages_view.addSubview(view);
        pin(view, &pages_view);
    }
    pages_view.addSubview(&empty.view);
    pages_view.addSubview(&hud.view);
    empty.view.setTranslatesAutoresizingMaskIntoConstraints(false);
    hud.view.setTranslatesAutoresizingMaskIntoConstraints(false);
    activate(&[
        empty.view.centerXAnchor().constraintEqualToAnchor(&pages_view.safeAreaLayoutGuide().centerXAnchor()),
        empty.view.centerYAnchor().constraintEqualToAnchor(&pages_view.centerYAnchor()),
        hud.view.centerXAnchor().constraintEqualToAnchor(&pages_view.safeAreaLayoutGuide().centerXAnchor()),
        hud.view.bottomAnchor().constraintEqualToAnchor_constant(&pages_view.bottomAnchor(), -12.0),
    ]);

    let detail = NSSplitView::new(mtm);
    detail.setVertical(false);
    detail.setDividerStyle(NSSplitViewDividerStyle::Thin);
    detail.addArrangedSubview(&pages_view);
    detail.addArrangedSubview(&logs.view);
    detail.setHoldingPriority_forSubviewAtIndex(250.0, 0);
    detail.setHoldingPriority_forSubviewAtIndex(260.0, 1);
    logs.view.setHidden(true);

    let sidebar = Sidebar::new(mtm, &actions);
    let sidebar_vc = NSViewController::new(mtm);
    sidebar_vc.setView(&sidebar.scroll);
    let sidebar_item = NSSplitViewItem::sidebarWithViewController(&sidebar_vc);
    sidebar_item.setMinimumThickness(190.0);
    sidebar_item.setMaximumThickness(280.0);
    let accessory = NSSplitViewItemAccessoryViewController::new(mtm);
    accessory.setView(&sidebar.accessory);
    sidebar_item.addBottomAlignedAccessoryViewController(&accessory);

    let detail_vc = NSViewController::new(mtm);
    detail_vc.setView(&detail);
    let detail_item = NSSplitViewItem::splitViewItemWithViewController(&detail_vc);
    detail_item.setMinimumThickness(560.0);
    // Lets the content run under the sidebar, which is what the glass refracts.
    // Without it the sidebar sits beside the content as a flat panel.
    detail_item.setAutomaticallyAdjustsSafeAreaInsets(true);

    let split = NSSplitViewController::new(mtm);
    split.addSplitViewItem(&sidebar_item);
    split.addSplitViewItem(&detail_item);

    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::FullSizeContentView;
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1120.0, 720.0));
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setContentViewController(Some(&split));
    window.setContentMinSize(NSSize::new(780.0, 500.0));
    window.setToolbarStyle(NSWindowToolbarStyle::Unified);
    let toolbar = Toolbar::new(mtm, &actions);
    window.setToolbar(Some(&toolbar.toolbar));
    window.setContentSize(frame.size);
    window.center();
    window.setFrameAutosaveName(&ns("DockerPlusMain"));
    if let Some(view) = window.contentView() {
        // TablePlus-style density rather than the roomier macOS 26 control metrics.
        view.setPrefersCompactControlSizeMetrics(true);
    }
    pages_view.setAdditionalSafeAreaInsets(NSEdgeInsets { top: 0.0, left: 0.0, bottom: HUD_CLEARANCE, right: 0.0 });

    let profile = saved_profile();
    let docker = Docker::new(Colima::default_socket(&profile));
    let ctl = Rc::new(Controller {
        mtm,
        window: window.clone(),
        actions,
        sidebar,
        toolbar,
        containers,
        images,
        volumes,
        networks,
        storage,
        colima_page,
        logs,
        hud,
        empty,
        detail,
        _split: split,
        colima: Colima::locate(&profile),
        docker: Arc::new(Mutex::new(docker)),
        st: RefCell::new(State { profile, ..State::default() }),
        #[cfg(feature = "devtools")]
        page_log: RefCell::default(),
    });
    CTL.with(|cell| cell.set(ctl.clone()).ok());
    window.makeKeyAndOrderFront(None);
    ctl.boot();
}

impl Controller {
    fn boot(&self) {
        self.sidebar.select(Page::Colima);
        self.render();
        self.reload_colima();
        spawn_watcher(self.docker.clone());
        self.watch_for_wake();
        self.tick();
    }

    fn docker(&self) -> Docker {
        self.docker.lock().unwrap().clone()
    }

    /// Whether a reply asked for in `epoch` still describes the engine on screen.
    fn current(&self, epoch: u64) -> bool {
        self.st.borrow().epoch == epoch
    }

    /// Starts over with the engine `self.docker` now points at: what the old one
    /// showed goes at once, and the watcher announces the new one, up or down.
    pub fn reconnect_engine(&self) {
        WATCH.restart();
        if self.st.borrow().engine_up {
            self.engine_changed(false);
        }
    }

    /// After the Mac sleeps, the event stream may be quietly dead. Reopening it
    /// also refetches every list, and Colima may have changed state meanwhile.
    fn woke(&self) {
        WATCH.interrupt();
        self.reload_colima();
    }

    fn watch_for_wake(&self) {
        let block = RcBlock::new(|_: NonNull<NSNotification>| {
            with_ctl(Controller::woke);
        });
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        // The center holds on to the observer, block included, for the app's life.
        let _ = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceDidWakeNotification),
                None,
                Some(&NSOperationQueue::mainQueue()),
                &block,
            )
        };
    }

    /// Colima for the profile on screen.
    fn colima(&self) -> Option<Colima> {
        self.colima.as_ref().map(|colima| colima.with_profile(&self.st.borrow().profile))
    }

    fn phase(&self) -> Phase {
        let st = self.st.borrow();
        match st.op {
            Some(ColimaOp::Stop | ColimaOp::Delete) => return Phase::Stopping,
            Some(ColimaOp::Start | ColimaOp::Restart | ColimaOp::Resize { .. }) => return Phase::Starting,
            None => {}
        }
        if st.engine_up {
            return Phase::Running;
        }
        match &st.info {
            None if self.colima.is_none() => Phase::NotInstalled,
            None => Phase::Unknown,
            Some(info) if !info.installed => Phase::NotInstalled,
            Some(info) if info.running => Phase::NoEngine,
            Some(_) => Phase::Stopped,
        }
    }

    pub fn handle(&self, msg: Msg) {
        match msg {
            Msg::Colima(number, profile, info) => {
                {
                    let mut st = self.st.borrow_mut();
                    if profile != st.profile || number < st.colima_shown {
                        return;
                    }
                    st.colima_shown = number;
                    st.colima_silent = info.is_none();
                }
                if let Some(info) = info {
                    if self.leave_vanished_profile(&info) {
                        return;
                    }
                    if let Some(socket) = info.status.as_ref().map(|s| s.docker_socket.as_str()) {
                        self.adopt_socket(socket);
                    }
                    self.st.borrow_mut().info = Some(*info);
                    self.render_chrome();
                }
                self.recheck_if_unsettled();
            }
            Msg::Engine(generation, up) => {
                // A report on an engine since swapped for another is stale.
                if generation == WATCH.generation() && up != self.st.borrow().engine_up {
                    self.engine_changed(up);
                }
            }
            Msg::Changed(scope) => self.schedule_refresh(scope, REFRESH_DEBOUNCE),
            Msg::Snapshot(epoch, snapshot) => self.snapshot_arrived(epoch, snapshot),
            Msg::Version(epoch, version) => {
                if self.current(epoch) {
                    self.st.borrow_mut().engine = version;
                    self.render_chrome();
                }
            }
            Msg::VolumeSizes(epoch, sizes) => {
                if self.current(epoch) {
                    self.st.borrow_mut().sizes = sizes;
                    self.render();
                }
            }
            Msg::CliLine(line) => self.colima_page.log_line(&line),
            Msg::CliDone(result) => {
                let op = self.st.borrow_mut().op.take();
                let deleted = op == Some(ColimaOp::Delete) && result.is_ok();
                if let Err(error) = result {
                    self.colima_page.log_line(&format!("Failed: {error}"));
                    self.show_error("Colima could not finish", &error);
                }
                if deleted {
                    self.select_profile(DEFAULT_PROFILE);
                }
                self.reload_colima();
                self.render_chrome();
            }
            Msg::ActionDone(error) => {
                {
                    let mut st = self.st.borrow_mut();
                    st.busy_actions = st.busy_actions.saturating_sub(1);
                }
                if let Some(error) = error {
                    self.show_error("Docker refused that", &error);
                }
                self.refresh(Scope::ALL);
                self.render_chrome();
            }
            Msg::PullProgress(text) => {
                self.st.borrow_mut().pull = Some(text);
                self.render_chrome();
            }
            Msg::PullDone(result) => {
                self.st.borrow_mut().pull = None;
                if let Err(error) = result {
                    self.show_error("Could not pull the image", &error);
                }
                self.refresh(Scope::IMAGES);
                self.render_chrome();
            }
            Msg::LogEnded(generation, error) => self.log_ended(generation, error),
            Msg::Details(result) => self.details_arrived(result),
            Msg::Storage(epoch, result, on_mac) => self.storage_arrived(epoch, result, on_mac),
            Msg::Pruned(result) => {
                {
                    let mut st = self.st.borrow_mut();
                    st.busy_actions = st.busy_actions.saturating_sub(1);
                }
                match result {
                    Ok(bytes) => {
                        let (title, detail) = pages::prune_result(bytes);
                        self.show_notice(&title, &detail);
                    }
                    Err(error) => self.show_error("Docker refused that", &error),
                }
                self.st.borrow_mut().vm_disk_read = None;
                self.check_vm_disk();
                self.refresh(Scope::ALL);
                self.render_chrome();
            }
            Msg::Stats(reading) => {
                if self.sampling(SampleKind::Containers) {
                    self.hud.set_stats(Some(&pages::stats_text(&self.stats_label(), &reading)));
                }
            }
            Msg::VmUsage(epoch, usage) => {
                if self.current(epoch) {
                    self.st.borrow_mut().vm_usage = usage;
                    self.render_chrome();
                }
            }
            Msg::PrunePreview(epoch, kind, result) => self.prune_preview_arrived(epoch, kind, result),
        }
    }

    fn adopt_socket(&self, socket: &str) {
        let socket = socket.trim_start_matches("unix://");
        if socket.is_empty() {
            return;
        }
        let mut docker = self.docker.lock().unwrap();
        if docker.socket().to_str() != Some(socket) {
            *docker = Docker::new(socket);
        }
    }

    fn reload_colima(&self) {
        let Some(colima) = self.colima() else {
            self.st.borrow_mut().info = Some(ColimaInfo::default());
            self.render_chrome();
            return;
        };
        let profile = colima.profile().to_string();
        let number = {
            let mut st = self.st.borrow_mut();
            st.colima_asked += 1;
            st.colima_asked
        };
        thread::spawn(move || post(Msg::Colima(number, profile, colima.info().map(Box::new))));
    }

    /// Nothing announces a VM going down when its socket has already gone, and
    /// Colima keeps listing it as running for a few seconds. Keep asking, less
    /// and less often, until it says stopped or Docker answers, so the UI cannot
    /// stick on "Waiting". A Colima that stopped answering gets the same.
    fn recheck_if_unsettled(&self) {
        let (delay, timer) = {
            let mut st = self.st.borrow_mut();
            let running = st.info.as_ref().is_some_and(|info| info.running);
            if !(running || st.colima_silent) || st.engine_up || st.op.is_some() {
                st.forget_rechecks();
                return;
            }
            if st.recheck_timer.is_some() {
                return;
            }
            st.recheck_timers += 1;
            st.recheck_timer = Some(st.recheck_timers);
            st.recheck_tries += 1;
            (recheck_delay(st.recheck_tries - 1), st.recheck_timers)
        };
        after_main(delay, move || {
            with_ctl(|ctl| {
                {
                    let mut st = ctl.st.borrow_mut();
                    if st.recheck_timer != Some(timer) {
                        return;
                    }
                    st.recheck_timer = None;
                }
                ctl.reload_colima();
            });
        });
    }

    fn engine_changed(&self, up: bool) {
        {
            let mut st = self.st.borrow_mut();
            st.engine_up = up;
            st.epoch += 1;
            if !up {
                st.containers.clear();
                st.images.clear();
                st.volumes.clear();
                st.networks.clear();
                st.sizes.clear();
                st.storage.clear();
                st.engine = None;
                st.vm_usage = VmUsage::default();
                st.vm_disk_read = None;
                st.retrying = false;
                st.storage_error = None;
                st.storage_again = false;
            }
        }
        if up {
            self.refresh(Scope::ALL);
            let docker = self.docker();
            let epoch = self.st.borrow().epoch;
            thread::spawn(move || post(Msg::Version(epoch, docker.version().ok())));
            // Open on Containers if Colima was already up at launch, but leave
            // someone watching the output of their own start where they are.
            let open_containers = {
                let mut st = self.st.borrow_mut();
                let open = !st.navigated && st.op.is_none();
                st.navigated = true;
                open
            };
            if open_containers {
                self.sidebar.select(Page::Containers);
            }
        } else {
            self.stop_logs();
        }
        self.reload_colima();
        self.sync_sampler();
        self.check_vm_disk();
        self.render();
    }

    /// One refresh at a time; lists asked for meanwhile wait for the next one.
    fn refresh(&self, scope: Scope) {
        let (scope, epoch) = {
            let mut st = self.st.borrow_mut();
            st.stale.merge(scope);
            if !st.engine_up || st.refreshing || st.stale.is_empty() {
                return;
            }
            st.refreshing = true;
            (std::mem::take(&mut st.stale), st.epoch)
        };
        let docker = self.docker();
        thread::spawn(move || post(Msg::Snapshot(epoch, docker.snapshot(scope))));
    }

    fn schedule_refresh(&self, scope: Scope, delay: Duration) {
        {
            let mut st = self.st.borrow_mut();
            st.stale.merge(scope);
            if st.refresh_scheduled {
                return;
            }
            st.refresh_scheduled = true;
        }
        after_main(delay, || {
            with_ctl(|ctl| {
                ctl.st.borrow_mut().refresh_scheduled = false;
                ctl.refresh(Scope::default());
            });
        });
    }

    fn snapshot_arrived(&self, epoch: u64, snapshot: Snapshot) {
        if !self.current(epoch) {
            // About an engine that has since gone or been swapped. What was asked
            // for meanwhile still has to be fetched.
            self.st.borrow_mut().refreshing = false;
            self.refresh(Scope::default());
            return;
        }
        let failed = snapshot.failed;
        let volumes_changed = {
            let mut st = self.st.borrow_mut();
            st.refreshing = false;
            st.stale.merge(failed);
            st.retrying = !failed.is_empty();
            if let Some(containers) = snapshot.containers {
                st.containers = containers;
            }
            if let Some(images) = snapshot.images {
                st.images = images;
            }
            if let Some(networks) = snapshot.networks {
                st.networks = networks;
            }
            let mut volumes_changed = false;
            if let Some(volumes) = snapshot.volumes {
                volumes_changed = st.volumes != volumes;
                st.volumes = volumes;
            }
            volumes_changed
        };
        self.render();
        if failed.is_empty() {
            self.refresh(Scope::default());
        } else {
            self.schedule_refresh(Scope::default(), REFRESH_RETRY);
        }
        if volumes_changed {
            self.load_volume_sizes();
        }
        self.load_storage();
        self.update_stats_targets();
    }

    /// Walks the disk, so it only runs for the page that shows it, one walk at a
    /// time; whatever asks meanwhile gets one more walk after it.
    fn load_storage(&self) {
        let epoch = {
            let mut st = self.st.borrow_mut();
            if st.page != Page::Storage || !st.engine_up {
                return;
            }
            if st.storage_loading {
                st.storage_again = true;
                return;
            }
            st.storage_loading = true;
            st.epoch
        };
        let docker = self.docker();
        let profile = self.st.borrow().profile.clone();
        thread::spawn(move || post(Msg::Storage(epoch, docker.disk_usage(), disk_on_mac(&profile))));
    }

    fn storage_arrived(&self, epoch: u64, result: DResult<Vec<StorageLine>>, on_mac: Option<u64>) {
        let again = {
            let mut st = self.st.borrow_mut();
            st.storage_loading = false;
            if st.epoch == epoch {
                match result {
                    Ok(lines) => {
                        st.storage = lines;
                        st.storage_error = None;
                    }
                    Err(error) => st.storage_error = Some(error),
                }
                st.mac_disk = on_mac;
            }
            std::mem::take(&mut st.storage_again)
        };
        self.render();
        if again {
            self.load_storage();
        }
    }

    fn load_volume_sizes(&self) {
        if self.st.borrow().page != Page::Volumes {
            return;
        }
        let docker = self.docker();
        let epoch = self.st.borrow().epoch;
        thread::spawn(move || {
            if let Ok(sizes) = docker.volume_sizes() {
                post(Msg::VolumeSizes(epoch, sizes));
            }
        });
    }

    fn tick(&self) {
        after_main(TICK, || {
            with_ctl(|ctl| {
                if NSApplication::sharedApplication(ctl.mtm).isActive() {
                    ctl.refresh(Scope::CONTAINERS);
                }
                ctl.tick();
            });
        });
    }

    // Rendering

    pub fn render(&self) {
        self.render_tables();
        self.render_chrome();
    }

    /// Everything but the table rows, for changes that leave the data alone.
    fn render_chrome(&self) {
        self.render_status();
        self.render_page();
        self.render_toolbar();
    }

    /// Only the page on screen gets rows; `show_page` renders the next one.
    pub(super) fn render_tables(&self) {
        let (table, rows) = {
            let st = self.st.borrow();
            let now = now();
            let query = st.filter.as_str();
            match st.page {
                Page::Colima => (None, Vec::new()),
                Page::Storage => (Some(&self.storage), pages::storage_rows(&st.storage)),
                Page::Containers => {
                    let shown = st.containers.iter().filter(|c| !st.running_only || c.is_running());
                    let rows = filter_rows(pages::container_rows(shown, now), query);
                    (Some(&self.containers), pages::group_by_project(rows, &st.containers, &st.collapsed))
                }
                Page::Images => {
                    (Some(&self.images), filter_rows(pages::image_rows(&st.images, &st.containers, now), query))
                }
                Page::Volumes => (
                    Some(&self.volumes),
                    filter_rows(pages::volume_rows(&st.volumes, &st.containers, &st.sizes, now), query),
                ),
                Page::Networks => {
                    (Some(&self.networks), filter_rows(pages::network_rows(&st.networks, &st.containers, now), query))
                }
            }
        };
        if let Some(table) = table {
            table.set_rows(rows);
        }

        let st = self.st.borrow();
        let up = st.engine_up;
        self.sidebar.set_count(Page::Containers, up.then_some(st.containers.len()));
        self.sidebar.set_count(Page::Images, up.then_some(st.images.len()));
        self.sidebar.set_count(Page::Volumes, up.then_some(st.volumes.len()));
        self.sidebar.set_count(Page::Networks, up.then_some(st.networks.len()));
    }

    fn render_status(&self) {
        let phase = self.phase();
        let st = self.st.borrow();
        let info = st.info.clone().unwrap_or_default();
        let (cpu, memory) = match (&info.status, &info.profile) {
            (Some(s), _) => (s.cpu, s.memory),
            (None, Some(p)) => (p.cpus, p.memory),
            _ => (0, 0),
        };
        let resources = if cpu > 0 { format!("{cpu} CPU · {}", human_gib(memory)) } else { String::new() };
        let stop = Some(("stop.fill", "Stop Colima", Action::ColimaStop));
        let start = Some(("play.fill", "Start Colima", Action::ColimaStart));

        let (title, detail, dot, busy, button) = match phase {
            Phase::Unknown => ("Checking…", String::new(), NSColor::tertiaryLabelColor(), true, None),
            Phase::NotInstalled => ("Not installed", "brew install colima".to_string(), NSColor::systemRedColor(), false, None),
            Phase::Stopped => ("Stopped", resources, NSColor::tertiaryLabelColor(), false, start),
            Phase::Starting => ("Starting…", String::new(), NSColor::systemOrangeColor(), true, None),
            Phase::Stopping => ("Stopping…", String::new(), NSColor::systemOrangeColor(), true, None),
            Phase::NoEngine => ("Waiting for Docker", resources, NSColor::systemOrangeColor(), true, stop),
            Phase::Running => ("Running", resources, NSColor::systemGreenColor(), false, stop),
        };
        self.sidebar.set_status(StatusView { title: title.to_string(), detail, dot, busy, button });

        let headline = match phase {
            Phase::NotInstalled => "Colima is not installed. Run: brew install colima".to_string(),
            Phase::Stopped if !info.provisioned => "No virtual machine yet. The first start downloads one.".to_string(),
            _ => String::new(),
        };
        let memory_in_use = st.vm_usage.memory.map(|(used, total)| pages::vm_usage_text(used, total));
        let disk_in_use = st.vm_usage.disk.map(|(used, total)| pages::vm_usage_text(used, total));
        let disk_warning = pages::vm_disk_warning(st.vm_usage.disk);
        let disk_in_use = disk_in_use.map(|text| if disk_warning.is_some() { format!("{text}, nearly full") } else { text });
        self.colima_page.update(&info, st.engine.as_ref(), memory_in_use, disk_in_use, &headline, title);

        let idle = st.op.is_none();
        let installed = self.colima.is_some();
        self.colima_page.set_profiles(&profile_choices(&info.profiles, &st.profile), &st.profile, installed && idle);
        let up = matches!(phase, Phase::Running | Phase::NoEngine);
        self.colima_page.start.setEnabled(installed && idle && !up);
        self.colima_page.stop.setEnabled(installed && idle && up);
        self.colima_page.restart.setEnabled(installed && idle && up);
        self.colima_page.resources.setEnabled(self.can(Action::ColimaResources));

        let (empty_title, empty_detail, show_start) = match phase {
            Phase::NotInstalled => ("Colima is not installed", "Install it with: brew install colima", false),
            Phase::Starting => ("Colima is starting", "This takes a minute, longer on the first run.", false),
            Phase::Stopping => ("Colima is stopping", "", false),
            Phase::NoEngine => ("Waiting for Docker", "Colima is up but its Docker socket has not answered yet.", false),
            Phase::Unknown => ("Checking Colima", "", false),
            Phase::Stopped | Phase::Running => ("Colima is stopped", "Start it to see your containers.", true),
        };
        self.empty.set(empty_title, empty_detail, show_start);
    }

    fn render_page(&self) {
        let st = self.st.borrow();
        let page = st.page;
        let up = st.engine_up;
        self.colima_page.view.setHidden(page != Page::Colima);
        self.containers.scroll.setHidden(page != Page::Containers || !up);
        self.images.scroll.setHidden(page != Page::Images || !up);
        self.volumes.scroll.setHidden(page != Page::Volumes || !up);
        self.networks.scroll.setHidden(page != Page::Networks || !up);
        self.storage.scroll.setHidden(page != Page::Storage || !up);
        self.empty.view.setHidden(page == Page::Colima || up);
        self.hud.view.setHidden(page == Page::Colima || !up);

        let summary = match page {
            Page::Colima => String::new(),
            Page::Containers => pages::containers_summary(&st.containers),
            Page::Images => pages::images_summary(&st.images),
            Page::Volumes => pages::volumes_summary(&st.volumes),
            Page::Networks => pages::networks_summary(&st.networks),
            Page::Storage => pages::storage_summary(&st.storage, st.mac_disk),
        };
        let summary = match pages::vm_disk_warning(st.vm_usage.disk) {
            Some(warning) if page != Page::Colima => format!("{warning}  ·  {summary}"),
            _ => summary,
        };
        self.window.setTitle(&ns(page.title()));
        self.window.setSubtitle(&ns(if up { &summary } else { "" }));

        let activity = st.pull.as_ref().map(|text| format!("Pull: {text}")).or_else(|| {
            (st.busy_actions > 0).then(|| "Working…".to_string())
        });
        let activity = activity.or_else(|| st.retrying.then(|| "Could not refresh, retrying…".to_string()));
        let activity = activity.or_else(|| {
            let error = st.storage_error.as_ref().filter(|_| page == Page::Storage)?;
            Some(format!("Could not read storage: {error}"))
        });
        self.hud.set_activity(activity.as_deref());
    }

    fn render_toolbar(&self) {
        let (page, up) = {
            let st = self.st.borrow();
            (st.page, st.engine_up)
        };
        let containers = page == Page::Containers;
        for action in [
            Action::ContainerStart,
            Action::ContainerStop,
            Action::ContainerRestart,
            Action::ShowLogs,
            Action::ComposeUp,
            Action::Pull,
            Action::RunImage,
            Action::Remove,
            Action::RunningOnly,
            Action::ToggleStats,
            Action::Prune,
            Action::Refresh,
        ] {
            let visible = match action {
                Action::ContainerStart
                | Action::ContainerStop
                | Action::ContainerRestart
                | Action::ShowLogs
                | Action::ComposeUp
                | Action::RunningOnly
                | Action::ToggleStats => containers,
                Action::Pull | Action::RunImage => page == Page::Images,
                Action::Prune => matches!(page, Page::Storage | Page::Networks),
                Action::Remove => matches!(page, Page::Containers | Page::Images | Page::Volumes | Page::Networks),
                _ => true,
            };
            self.toolbar.set(action, visible && up, self.can(action));
        }
        self.toolbar.set_search_visible(matches!(page, Page::Containers | Page::Images | Page::Volumes | Page::Networks) && up);
    }

    /// Whether an action applies right now. Drives both toolbar and menus.
    pub fn can(&self, action: Action) -> bool {
        let phase = self.phase();
        let st = self.st.borrow();
        let ids = self.selected_ids();
        let selected: Vec<&Container> = ids.iter().filter_map(|key| st.containers.iter().find(|c| c.id == *key)).collect();
        match action {
            Action::ColimaStart => phase == Phase::Stopped,
            Action::ColimaStop | Action::ColimaRestart => matches!(phase, Phase::Running | Phase::NoEngine),
            Action::ContainerStart => selected.iter().any(|c| !c.is_running()),
            Action::ContainerStop => selected.iter().any(|c| c.is_running()),
            Action::ContainerRestart => !selected.is_empty(),
            Action::ShowLogs => selected.len() == 1 && self.selected_projects().is_empty(),
            Action::Prune if st.page == Page::Networks => !pages::unused_networks(&st.networks, &st.containers).is_empty(),
            Action::Prune => self.selected_storage_line().is_some_and(|l| l.reclaimable > 0 || l.items > l.active),
            Action::ToggleStats => st.engine_up,
            Action::LogsFind => !self.logs.view.isHidden(),
            // Not while half-started: the sizes Colima reports then are unreliable.
            Action::ColimaResources => self.colima.is_some() && matches!(phase, Phase::Stopped | Phase::Running),
            Action::ColimaProfile | Action::ColimaNewProfile | Action::ColimaDeleteProfile => {
                self.colima.is_some() && st.op.is_none()
            }
            Action::ContainerPause => selected.iter().any(|c| c.state == "running"),
            Action::ContainerResume => selected.iter().any(|c| c.state == "paused"),
            Action::ShowDetails => st.page == Page::Containers && selected.len() == 1,
            Action::OpenTerminal => matches!(selected.as_slice(), [c] if c.state == "running"),
            Action::OpenPort => matches!(selected.as_slice(), [c] if !pages::port_links(&c.ports).is_empty()),
            Action::RevealMount => matches!(selected.as_slice(), [c] if !host_mounts(c).is_empty()),
            Action::SelectProject => selected.iter().any(|c| !c.project().is_empty()),
            Action::RunningOnly => st.engine_up,
            Action::CheckUpdates => !super::updates::checking(),
            Action::Pull | Action::RunImage | Action::Refresh | Action::ComposeUp => st.engine_up,
            Action::Remove if st.page == Page::Networks => !self.removable_networks().is_empty(),
            Action::Remove | Action::CopyId => match st.page {
                Page::Colima | Page::Storage => false,
                Page::Containers => !selected.is_empty(),
                _ => !self.current_table().is_some_and(|t| t.selected_keys().is_empty()),
            },
            _ => true,
        }
    }

    fn current_table(&self) -> Option<&Table> {
        match self.st.borrow().page {
            Page::Colima | Page::Storage => None,
            Page::Containers => Some(&self.containers),
            Page::Images => Some(&self.images),
            Page::Volumes => Some(&self.volumes),
            Page::Networks => Some(&self.networks),
        }
    }

    /// The containers the selection stands for: a project header means all of
    /// its containers, including any folded away or hidden by a filter.
    fn selected_ids(&self) -> Vec<String> {
        let st = self.st.borrow();
        let mut ids = Vec::new();
        for key in self.containers.selected_keys() {
            match pages::project_of_key(&key) {
                Some(project) => {
                    ids.extend(st.containers.iter().filter(|c| c.project() == project).map(|c| c.id.clone()));
                }
                None => ids.push(key),
            }
        }
        ids.sort();
        ids.dedup();
        ids
    }

    /// The projects selected, when nothing but project headers is.
    fn selected_projects(&self) -> Vec<String> {
        let keys = self.containers.selected_keys();
        let projects: Vec<String> = keys.iter().filter_map(|key| pages::project_of_key(key)).map(String::from).collect();
        if projects.len() == keys.len() { projects } else { Vec::new() }
    }

    fn selected_storage_line(&self) -> Option<StorageLine> {
        let keys = self.storage.selected_keys();
        let [key] = keys.as_slice() else { return None };
        self.st.borrow().storage.iter().find(|line| line.kind.title() == key).copied()
    }

    /// The selected networks, less the engine's own.
    fn removable_networks(&self) -> Vec<String> {
        let st = self.st.borrow();
        let keys = self.networks.selected_keys();
        st.networks.iter().filter(|n| !n.is_builtin() && keys.contains(&n.id)).map(|n| n.id.clone()).collect()
    }

    // User input

    pub fn show_page(&self, page: Page) {
        #[cfg(feature = "devtools")]
        self.note_page_change(page);
        self.st.borrow_mut().page = page;
        if page == Page::Volumes {
            self.load_volume_sizes();
        }
        self.load_storage();
        self.sync_sampler();
        self.render();
    }

    /// Folds the project whose disclosure arrow this is.
    pub fn arrow_clicked(&self, arrow: &NSView) {
        let key = self.containers.key_of_row_holding(arrow);
        if let Some(project) = key.as_deref().and_then(pages::project_of_key) {
            self.toggle_project(project);
        }
    }

    fn toggle_project(&self, project: &str) {
        {
            let mut st = self.st.borrow_mut();
            if !st.collapsed.remove(project) {
                st.collapsed.insert(project.to_string());
            }
        }
        self.render_tables();
    }

    pub fn selection_changed(&self) {
        self.render_toolbar();
        self.update_stats_targets();
        // With the panel open, the logs follow the selection.
        let target = {
            let st = self.st.borrow();
            if st.page != Page::Containers || st.logs.is_none() {
                return;
            }
            let keys = self.selected_ids();
            match keys.as_slice() {
                [only] if st.logs.as_ref().is_none_or(|s| s.container != *only) => Some(only.clone()),
                _ => None,
            }
        };
        if let Some(id) = target {
            self.open_logs(&id);
        }
    }

    pub fn perform(&self, action: Action) {
        match action {
            Action::ColimaStart => self.colima_op(ColimaOp::Start),
            Action::ColimaStop => self.confirm_colima(ColimaOp::Stop),
            Action::ColimaRestart => self.confirm_colima(ColimaOp::Restart),
            Action::ContainerStart => self.container_action(ContainerAction::Start),
            Action::ContainerStop => self.container_action(ContainerAction::Stop),
            Action::ContainerRestart => self.container_action(ContainerAction::Restart),
            Action::Remove => self.confirm_remove(),
            Action::ShowLogs => {
                if self.st.borrow().page == Page::Containers {
                    // Double-clicking a project folds it; the Logs button stays
                    // disabled for one, so only that path gets here.
                    if let [project] = self.selected_projects().as_slice() {
                        self.toggle_project(project);
                    } else if let [id] = self.selected_ids().as_slice() {
                        self.open_logs(id);
                    }
                }
            }
            Action::Pull => self.ask_pull(),
            Action::Refresh => {
                self.refresh(Scope::ALL);
                self.reload_colima();
                self.load_volume_sizes();
                self.load_storage();
            }
            Action::CopyId => self.copy_selection(),
            Action::FocusSearch => self.toolbar.focus_search(),
            Action::Filter => {
                self.st.borrow_mut().filter = self.toolbar.search_text();
                self.render_tables();
            }
            Action::ShowColima => self.sidebar.select(Page::Colima),
            Action::ShowContainers => self.sidebar.select(Page::Containers),
            Action::ShowImages => self.sidebar.select(Page::Images),
            Action::ShowVolumes => self.sidebar.select(Page::Volumes),
            Action::ShowNetworks => self.sidebar.select(Page::Networks),
            Action::ShowStorage => self.sidebar.select(Page::Storage),
            Action::Prune => self.ask_prune(),
            Action::ComposeUp => self.ask_compose_up(),
            Action::RunImage => self.ask_run_image(),
            Action::LogsFind => {
                if !self.logs.view.isHidden() {
                    self.logs.show_find(&self.window, self.mtm);
                }
            }
            Action::ToggleStats => self.toggle_stats(),
            Action::LogsFollow => self.logs.toggle_follow(),
            Action::LogsClear => self.logs.clear(),
            Action::LogsClose => self.close_logs(),
            Action::ColimaResources => self.ask_resources(),
            Action::ColimaProfile => {
                let pick = self.colima_page.picked_profile();
                // The last two entries are not profiles: show the current one again.
                self.render_status();
                match pick {
                    ProfilePick::Profile(name) => self.select_profile(&name),
                    ProfilePick::New => self.ask_new_profile(NewProfile::default()),
                    ProfilePick::Delete => self.ask_delete_profile(),
                }
            }
            Action::ColimaNewProfile => self.ask_new_profile(NewProfile::default()),
            Action::ColimaDeleteProfile => self.ask_delete_profile(),
            Action::ColimaConfig => self.open_colima_config(),
            Action::ContainerPause => self.container_action(ContainerAction::Pause),
            Action::ContainerResume => self.container_action(ContainerAction::Unpause),
            Action::ShowDetails => self.show_details(),
            Action::OpenTerminal => self.open_terminal(),
            Action::CheckUpdates => super::updates::check(),
            Action::SelectProject => self.select_project(),
            Action::RunningOnly => {
                let on = !self.st.borrow().running_only;
                self.set_running_only(on);
                self.render();
            }
            // These open through the submenus that menu_will_open fills in.
            Action::OpenPort | Action::RevealMount => {}
        }
    }

    // Colima

    fn confirm_colima(&self, op: ColimaOp) {
        let (running, profile) = {
            let st = self.st.borrow();
            (st.containers.iter().filter(|c| c.is_running()).count(), st.profile.clone())
        };
        let (verb, button) = if op == ColimaOp::Stop { ("Stop", "Stop") } else { ("Restart", "Restart") };
        let detail = running_note(running);
        self.confirm(&format!("{verb} Colima profile “{profile}”?"), &detail, button, move |ctl| ctl.colima_op(op));
    }

    fn colima_op(&self, op: ColimaOp) {
        let Some(colima) = self.colima() else { return };
        {
            let mut st = self.st.borrow_mut();
            if st.op.is_some() {
                return;
            }
            st.op = Some(op);
        }
        self.colima_page.clear_output();
        self.render_chrome();
        let on_line = |line| post(Msg::CliLine(line));
        thread::spawn(move || {
            let result = match op {
                ColimaOp::Start => colima.start(ResourceChange::default(), on_line),
                ColimaOp::Stop => colima.stop(on_line),
                // A stop closes the VM's ssh connection, which a delete leaves behind.
                ColimaOp::Delete => colima.stop(on_line).and_then(|()| colima.delete(on_line)),
                // `colima restart` takes no --activate flag, and a plain start
                // may switch the docker context, so this is spelled out.
                ColimaOp::Restart => {
                    colima.stop(on_line).and_then(|()| colima.start(ResourceChange::default(), on_line))
                }
                ColimaOp::Resize { change, restart } => {
                    let stopped = if restart { colima.stop(on_line) } else { Ok(()) };
                    stopped.and_then(|()| colima.start(change, on_line))
                }
            };
            post(Msg::CliDone(result));
        });
    }

    fn ask_delete_profile(&self) {
        if self.colima.is_none() || self.st.borrow().op.is_some() {
            return;
        }
        let profile = self.st.borrow().profile.clone();
        let detail = "Its VM goes, with every container, image and volume in it. This cannot be undone.";
        self.confirm(&format!("Delete profile “{profile}”?"), detail, "Delete", |ctl| ctl.colima_op(ColimaOp::Delete));
    }

    /// In a text editor, whatever the Mac has set for YAML. The sheet that
    /// offers it closes: an edit takes effect at the next start, not through it.
    fn open_colima_config(&self) {
        if let Some(sheet) = self.window.attachedSheet() {
            self.window.endSheet_returnCode(&sheet, NSModalResponseCancel);
        }
        let Some(path) = config_file(&self.st.borrow().profile) else { return };
        // Waited for off the main thread, so it does not stay behind as a zombie.
        thread::spawn(move || Command::new("/usr/bin/open").arg("-t").arg(&path).status());
    }

    /// Asks for a new VM size, which is all most people ever change. Colima only
    /// applies it on start, so an up VM is restarted, with the same warning as a
    /// plain restart. Everything else is in `colima.yaml`, one click from here.
    fn ask_resources(&self) {
        let up = self.phase() == Phase::Running;
        let (current, running, profile) = {
            let st = self.st.borrow();
            let current = st.info.as_ref().and_then(Resources::current);
            (current, st.containers.iter().filter(|c| c.is_running()).count(), st.profile.clone())
        };
        let chosen = current.unwrap_or(Resources::DEFAULT);
        let host = NSProcessInfo::processInfo();
        let cpu_values: Vec<u32> = (1..=(host.processorCount() as u32).max(chosen.cpus)).collect();
        let memory_values = Resources::memory_choices((host.physicalMemory() >> 30) as u32, chosen.memory);
        let disk_values = Resources::disk_choices(current.map(|r| r.disk));

        let cpus = choice_popup(self.mtm, &cpu_values, "", chosen.cpus);
        let memory = choice_popup(self.mtm, &memory_values, " GiB", chosen.memory);
        let disk = choice_popup(self.mtm, &disk_values, " GiB", chosen.disk);
        let grid = NSGridView::gridViewWithNumberOfColumns_rows(2, 0, self.mtm);
        grid.setRowSpacing(8.0);
        grid.setColumnSpacing(10.0);
        grid.setRowAlignment(NSGridRowAlignment::FirstBaseline);
        for (title, popup) in [("CPUs", &cpus), ("Memory", &memory), ("Disk", &disk)] {
            let key = label(self.mtm, title, 13.0, unsafe { NSFontWeightRegular });
            let row: [&NSView; 2] = [&key, popup];
            grid.addRowWithViews(&NSArray::from_slice(&row));
            activate(&[popup.widthAnchor().constraintEqualToConstant(120.0)]);
        }
        let add_row = |view: &NSView| {
            let key = label(self.mtm, "", 13.0, unsafe { NSFontWeightRegular });
            let row: [&NSView; 2] = [&key, view];
            grid.addRowWithViews(&NSArray::from_slice(&row));
        };
        add_row(&secondary_label(self.mtm, "Disk size can only grow.", 11.0));
        if config_file(&profile).is_some() {
            let edit = unsafe {
                NSButton::buttonWithTitle_target_action(&ns("Edit colima.yaml…"), Some(&*self.actions), Some(sel!(dpPerform:)), self.mtm)
            };
            edit.setTag(Action::ColimaConfig as NSInteger);
            edit.setControlSize(NSControlSize::Small);
            add_row(&edit);
        }
        grid.columnAtIndex(0).setXPlacement(NSGridCellPlacement::Trailing);
        grid.setFrameSize(grid.fittingSize());

        let detail = if up {
            format!("Colima restarts to apply changes. {}", running_note(running))
        } else {
            "Colima starts with these settings.".to_string()
        };
        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&ns(&format!("Settings for “{profile}”")));
        alert.setInformativeText(&ns(&detail));
        alert.addButtonWithTitle(&ns(if up { "Restart" } else { "Start" }));
        alert.addButtonWithTitle(&ns("Cancel"));
        alert.setAccessoryView(Some(&grid));
        let block = RcBlock::new(move |response: NSModalResponse| {
            let pick = |popup: &NSPopUpButton, values: &[u32]| values.get(popup.indexOfSelectedItem() as usize).copied();
            let picked = (pick(&cpus, &cpu_values), pick(&memory, &memory_values), pick(&disk, &disk_values));
            let (Some(c), Some(m), Some(d)) = picked else { return };
            if response == NSAlertFirstButtonReturn {
                with_ctl(|ctl| ctl.apply_resources(Resources { cpus: c, memory: m, disk: d }));
            }
        });
        alert.beginSheetModalForWindow_completionHandler(&self.window, Some(&block));
    }

    /// Judged now, not when the sheet opened: Colima may have been started or
    /// stopped from a terminal in between, and a start on a running VM would
    /// quietly ignore the new size.
    fn apply_resources(&self, wanted: Resources) {
        let up = matches!(self.phase(), Phase::Running | Phase::NoEngine);
        let current = self.st.borrow().info.as_ref().and_then(Resources::current);
        let change = ResourceChange::between(current, wanted);
        if !(up && change.is_empty()) {
            self.colima_op(ColimaOp::Resize { change, restart: up });
        }
    }

    /// Points the app at another profile: its VM, its engine and its disk.
    pub fn select_profile(&self, name: &str) {
        {
            let mut st = self.st.borrow_mut();
            if st.profile == name || st.op.is_some() {
                return;
            }
            st.profile = name.to_string();
            st.info = None;
            st.colima_silent = false;
            st.forget_rechecks();
            st.vm_usage = VmUsage::default();
            st.vm_disk_read = None;
            // It holds the old profile's Colima; sync_sampler starts a new one.
            if let Some(sampler) = st.sampler.take() {
                sampler.stop.store(true, Ordering::Relaxed);
            }
        }
        unsafe { NSUserDefaults::standardUserDefaults().setObject_forKey(Some(&ns(name)), &ns(PROFILE_KEY)) };
        *self.docker.lock().unwrap() = Docker::new(Colima::default_socket(name));
        self.close_logs();
        self.colima_page.clear_output();
        self.reconnect_engine();
        self.reload_colima();
        self.sync_sampler();
        self.render();
    }

    /// At launch, a saved profile whose VM has since been deleted gives way to
    /// the default one. An empty list may only mean `colima list` failed, so
    /// it moves nothing.
    fn leave_vanished_profile(&self, info: &ColimaInfo) -> bool {
        let gone = {
            let mut st = self.st.borrow_mut();
            let first = !std::mem::replace(&mut st.profile_checked, true);
            first && st.profile != DEFAULT_PROFILE && !info.profiles.is_empty() && !info.profiles.contains(&st.profile)
        };
        if gone {
            self.select_profile(DEFAULT_PROFILE);
        }
        gone
    }

    /// Asks for a new profile. Its VM type and mount are fixed once Colima
    /// creates the VM, so this is the one place to choose them.
    fn ask_new_profile(&self, draft: NewProfile) {
        if self.colima.is_none() || self.st.borrow().op.is_some() {
            return;
        }
        let host = NSProcessInfo::processInfo();
        let size = draft.size;
        let cpu_values: Vec<u32> = (1..=(host.processorCount() as u32).max(size.cpus)).collect();
        let memory_values = Resources::memory_choices((host.physicalMemory() >> 30) as u32, size.memory);
        let disk_values = Resources::disk_choices(None);

        let name = NSTextField::textFieldWithString(&ns(&draft.name), self.mtm);
        name.setPlaceholderString(Some(&ns("letters, digits, - and _")));
        activate(&[name.widthAnchor().constraintEqualToConstant(220.0)]);
        let cpus = choice_popup(self.mtm, &cpu_values, "", size.cpus);
        let memory = choice_popup(self.mtm, &memory_values, " GiB", size.memory);
        let disk = choice_popup(self.mtm, &disk_values, " GiB", size.disk);
        let kind = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(self.mtm), NSRect::ZERO, false);
        for vm in VM_KINDS {
            kind.addItemWithTitle(&ns(vm.title));
        }
        kind.selectItemAtIndex(draft.kind as NSInteger);
        let rosetta = checkbox(self.mtm, "Rosetta for x86 images (VZ only)", draft.rosetta);
        let kubernetes = checkbox(self.mtm, "Kubernetes", draft.kubernetes);

        let grid = NSGridView::gridViewWithNumberOfColumns_rows(2, 0, self.mtm);
        grid.setRowSpacing(8.0);
        grid.setColumnSpacing(10.0);
        let add_row = |title: &str, view: &NSView| {
            let key = label(self.mtm, title, 13.0, unsafe { NSFontWeightRegular });
            let row: [&NSView; 2] = [&key, view];
            grid.addRowWithViews(&NSArray::from_slice(&row));
        };
        add_row("Name", &name);
        add_row("CPUs", &cpus);
        add_row("Memory", &memory);
        add_row("Disk", &disk);
        add_row("Virtual machine", &kind);
        if cfg!(target_arch = "aarch64") {
            add_row("", &rosetta);
        }
        add_row("", &kubernetes);
        grid.columnAtIndex(0).setXPlacement(NSGridCellPlacement::Trailing);
        grid.setFrameSize(grid.fittingSize());

        let detail = draft.problem.clone().unwrap_or_else(|| {
            "A separate VM with its own Docker engine, images and volumes. Its VM type and mount cannot be \
             changed later. Kubernetes takes more CPU and memory in the VM."
                .to_string()
        });
        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&ns("New Colima profile"));
        alert.setInformativeText(&ns(&detail));
        alert.addButtonWithTitle(&ns("Create and Start"));
        alert.addButtonWithTitle(&ns("Cancel"));
        alert.setAccessoryView(Some(&grid));
        alert.window().setInitialFirstResponder(Some(&name));
        let block = RcBlock::new(move |response: NSModalResponse| {
            if response != NSAlertFirstButtonReturn {
                return;
            }
            let pick = |popup: &NSPopUpButton, values: &[u32]| values.get(popup.indexOfSelectedItem() as usize).copied();
            let picked = (pick(&cpus, &cpu_values), pick(&memory, &memory_values), pick(&disk, &disk_values));
            let (Some(c), Some(m), Some(d)) = picked else { return };
            let draft = NewProfile {
                name: name.stringValue().to_string().trim().to_string(),
                size: Resources { cpus: c, memory: m, disk: d },
                kind: (kind.indexOfSelectedItem().max(0) as usize).min(VM_KINDS.len() - 1),
                rosetta: cfg!(target_arch = "aarch64") && is_on(&rosetta),
                kubernetes: is_on(&kubernetes),
                problem: None,
            };
            with_ctl(|ctl| ctl.create_profile(draft));
        });
        alert.beginSheetModalForWindow_completionHandler(&self.window, Some(&block));
    }

    /// Switches to the new profile and starts its VM, with the output on the
    /// Colima page as for any start. A bad name brings the sheet back.
    fn create_profile(&self, draft: NewProfile) {
        let problem = {
            let st = self.st.borrow();
            let existing = st.info.as_ref().map(|info| info.profiles.clone()).unwrap_or_default();
            profile_name_problem(&draft.name, &existing)
        };
        if let Some(problem) = problem {
            // After this sheet has gone, so the next one can attach.
            after_main(Duration::ZERO, move || {
                with_ctl(|ctl| ctl.ask_new_profile(NewProfile { problem: Some(problem), ..draft }));
            });
            return;
        }
        if self.st.borrow().op.is_some() {
            return;
        }
        let change = ResourceChange::new_vm(draft.size, VM_KINDS[draft.kind], draft.rosetta, draft.kubernetes);
        self.select_profile(&draft.name);
        self.sidebar.select(Page::Colima);
        self.colima_op(ColimaOp::Resize { change, restart: false });
    }

    // Containers, images and volumes

    fn container_action(&self, action: ContainerAction) {
        let verb = match action {
            ContainerAction::Start => Some("start"),
            ContainerAction::Stop => Some("stop"),
            ContainerAction::Restart => Some("restart"),
            ContainerAction::Pause | ContainerAction::Unpause => None,
        };
        let projects = self.selected_projects();
        if let (Some(verb), false) = (verb, projects.is_empty()) {
            return self.compose_action(projects, verb);
        }
        let keys = self.selected_ids();
        let targets: Vec<(String, String)> = {
            let st = self.st.borrow();
            let applies = |c: &Container| match action {
                ContainerAction::Start => !c.is_running(),
                ContainerAction::Stop => c.is_running(),
                ContainerAction::Restart => true,
                ContainerAction::Pause => c.state == "running",
                ContainerAction::Unpause => c.state == "paused",
            };
            keys.into_iter()
                .filter_map(|key| {
                    let container = st.containers.iter().find(|c| c.id == key && applies(c))?;
                    Some((container.name().to_string(), key))
                })
                .collect()
        };
        if targets.is_empty() {
            return;
        }
        self.run_each(targets, move |docker, id| docker.container_action(id, action));
    }

    /// Acts on whole projects through `docker compose`, which knows the order
    /// their services depend on.
    fn compose_action(&self, projects: Vec<String>, verb: &'static str) {
        let items = projects.into_iter().map(|project| (project.clone(), project)).collect();
        self.run_each(items, move |docker, project| compose(docker.socket(), project, &[verb]));
    }

    /// Asks for a project folder or a Compose file. Files of any type can be
    /// picked; compose_up_args turns down the ones that are not YAML.
    fn ask_compose_up(&self) {
        let panel = NSOpenPanel::openPanel(self.mtm);
        panel.setCanChooseDirectories(true);
        panel.setCanChooseFiles(true);
        panel.setAllowsMultipleSelection(false);
        panel.setMessage(Some(&ns("Choose a project folder or a Compose file to start on Colima.")));
        panel.setPrompt(Some(&ns("Compose Up")));
        let chosen = panel.clone();
        let block = RcBlock::new(move |response: NSModalResponse| {
            // The panel may still be on screen, and an alert may follow.
            chosen.orderOut(None);
            let url = chosen.URLs().firstObject();
            let path = url.and_then(|url| url.path()).map(|path| PathBuf::from(path.to_string()));
            if let (true, Some(path)) = (response == NSModalResponseOK, path) {
                with_ctl(|ctl| ctl.compose_up(path));
            }
        });
        panel.beginSheetModalForWindow_completionHandler(&self.window, &block);
    }

    fn compose_up(&self, path: PathBuf) {
        if let Err(error) = compose_up_args(&path, path.is_dir()) {
            return self.show_error("Not a Compose project", &error);
        }
        let home = env::var_os("HOME").map(PathBuf::from);
        if home.is_some_and(|home| !shared_with_vm(&path, &home)) {
            let detail = format!(
                "{}\n\nColima shares only your home folder with its VM, so bind mounts from here come up empty.",
                path.display()
            );
            return self.confirm("This project is outside your home folder", &detail, "Compose Up", move |ctl| {
                ctl.run_docker(move |docker| compose_up(docker.socket(), &path).err());
            });
        }
        self.run_docker(move |docker| compose_up(docker.socket(), &path).err());
    }

    /// Runs engine work off the main thread, tracked by the activity capsule.
    fn run_docker(&self, work: impl FnOnce(&Docker) -> Option<String> + Send + 'static) {
        self.st.borrow_mut().busy_actions += 1;
        self.render_chrome();
        let docker = self.docker();
        thread::spawn(move || post(Msg::ActionDone(work(&docker))));
    }

    /// Runs `work` on every item off the main thread and reports every failure at
    /// the end, so one refusal does not leave the rest of a batch undone. Each
    /// item is a name to show and the key to act on.
    fn run_each(&self, items: Vec<(String, String)>, work: impl Fn(&Docker, &str) -> DResult<()> + Send + 'static) {
        self.run_docker(move |docker| {
            let failures: Vec<(String, String)> = items
                .iter()
                .filter_map(|(name, key)| work(docker, key).err().map(|error| (name.clone(), error)))
                .collect();
            pages::batch_error(items.len(), &failures)
        });
    }

    fn confirm_remove(&self) {
        let page = self.st.borrow().page;
        let projects = self.selected_projects();
        if page == Page::Containers && !projects.is_empty() {
            let count = self.selected_ids().len();
            let title = match projects.as_slice() {
                [project] => format!("Remove project “{project}”?"),
                _ => format!("Remove {} projects?", projects.len()),
            };
            let detail = format!(
                "{count} {} and the project's networks go. Volumes and images stay.",
                pages::plural(count, "container")
            );
            return self.confirm(&title, &detail, "Remove", move |ctl| ctl.compose_action(projects, "down"));
        }
        let Some(table) = self.current_table() else { return };
        let keys = match page {
            Page::Containers => self.selected_ids(),
            Page::Networks => self.removable_networks(),
            _ => table.selected_keys(),
        };
        if keys.is_empty() {
            return;
        }
        let count = keys.len();
        let items: Vec<(String, String)> = {
            let st = self.st.borrow();
            keys.iter()
                .map(|key| {
                    let name = match page {
                        Page::Containers => {
                            st.containers.iter().find(|c| c.id == *key).map_or(key.clone(), |c| c.name().to_string())
                        }
                        Page::Networks => st.networks.iter().find(|n| n.id == *key).map_or(key.clone(), |n| n.name.clone()),
                        _ => key.clone(),
                    };
                    (name, key.clone())
                })
                .collect()
        };
        let names: Vec<&str> = items.iter().map(|(name, _)| name.as_str()).take(5).collect();
        let any_running = {
            let st = self.st.borrow();
            page == Page::Containers
                && keys.iter().any(|key| st.containers.iter().any(|c| c.id == *key && c.is_running()))
        };
        let noun = match page {
            Page::Containers => "container",
            Page::Images => "image",
            Page::Networks => "network",
            _ => "volume",
        };
        let mut detail = names.join(", ");
        if count > names.len() {
            detail.push_str(&format!(" and {} more", count - names.len()));
        }
        match page {
            Page::Containers if any_running => detail.push_str("\n\nRunning containers are killed first."),
            Page::Volumes => detail.push_str("\n\nThe data in a removed volume cannot be recovered."),
            _ => {}
        }
        let title = if count == 1 { format!("Remove this {noun}?") } else { format!("Remove {count} {noun}s?") };
        self.confirm(&title, &detail, "Remove", move |ctl| {
            ctl.run_each(items, move |docker, key| match page {
                Page::Containers => docker.remove_container(key, true),
                Page::Images => docker.remove_image(key),
                Page::Networks => docker.remove_network(key),
                _ => docker.remove_volume(key),
            });
        });
    }

    /// Asks the engine what would go before asking the user: the page may be
    /// minutes old, and the prompt names everything about to be deleted.
    fn ask_prune(&self) {
        if self.st.borrow().page == Page::Networks {
            return self.ask_prune_networks();
        }
        let Some(kind) = self.selected_storage_line().map(|line| line.kind) else { return };
        let epoch = {
            let mut st = self.st.borrow_mut();
            st.busy_actions += 1;
            st.epoch
        };
        self.render_chrome();
        let docker = self.docker();
        thread::spawn(move || post(Msg::PrunePreview(epoch, kind, docker.prune_preview(kind))));
    }

    fn prune_preview_arrived(&self, epoch: u64, kind: Prune, result: DResult<PrunePreview>) {
        {
            let mut st = self.st.borrow_mut();
            st.busy_actions = st.busy_actions.saturating_sub(1);
        }
        self.render_chrome();
        if !self.current(epoch) {
            return;
        }
        match result.map(|preview| (pages::prune_prompt(kind, &preview), preview)) {
            Ok((Some((title, detail)), preview)) => {
                self.confirm(&title, &detail, "Remove", move |ctl| ctl.prune(kind, preview));
            }
            Ok((None, _)) => {
                let (title, detail) = pages::prune_nothing(kind);
                self.show_notice(&title, &detail);
            }
            Err(error) => self.show_error("Could not check what would be removed", &error),
        }
    }

    /// Removes the networks it names one by one, so nothing goes that the
    /// confirmation did not list.
    fn ask_prune_networks(&self) {
        let (items, prompt) = {
            let st = self.st.borrow();
            let unused = pages::unused_networks(&st.networks, &st.containers);
            let items: Vec<(String, String)> = unused.iter().map(|n| (n.name.clone(), n.id.clone())).collect();
            (items, pages::network_prune_prompt(&unused))
        };
        if items.is_empty() {
            return self.show_notice("Nothing to remove", "Every network has a container attached.");
        }
        self.confirm(&prompt.0, &prompt.1, "Remove", move |ctl| {
            ctl.run_each(items, |docker, id| docker.remove_network(id));
        });
    }

    /// Removes what `preview` listed, the list the user just confirmed.
    fn prune(&self, kind: Prune, preview: PrunePreview) {
        self.st.borrow_mut().busy_actions += 1;
        self.render_chrome();
        let docker = self.docker();
        thread::spawn(move || {
            let before = docker.disk_usage().unwrap_or_default();
            let removed = if kind == Prune::BuildCache {
                docker.clear_build_cache()
            } else {
                let refused = docker.remove_listed(kind, &preview.items);
                pages::batch_error(preview.items.len(), &refused).map_or(Ok(preview.size), Err)
            };
            let result = removed.map(|expected| {
                let after = docker.disk_usage().unwrap_or_default();
                freed_bytes(kind, &before, &after, expected)
            });
            post(Msg::Pruned(result));
        });
    }

    fn ask_run_image(&self) {
        let image = self.current_table().and_then(|table| table.selected_keys().into_iter().next()).unwrap_or_default();
        self.show_run_form(RunForm { image, ..RunForm::default() }, None);
    }

    /// A few fields for the common `docker run`. Anything more is for the command
    /// line. A form that cannot work comes back with the reason.
    fn show_run_form(&self, form: RunForm, problem: Option<String>) {
        let field = |value: &str, placeholder: &str| {
            let frame = NSRect::new(NSPoint::ZERO, NSSize::new(280.0, 22.0));
            let field = NSTextField::initWithFrame(NSTextField::alloc(self.mtm), frame);
            field.setStringValue(&ns(value));
            field.setPlaceholderString(Some(&ns(placeholder)));
            field
        };
        let image = field(&form.image, "nginx:alpine");
        let name = field(&form.name, "Optional");
        let ports = field(&form.ports, "8080:80 8443:443");
        let env = field(&form.env, "MODE=dev DEBUG=1");
        let volumes = field(&form.volumes, "data:/var/lib/data ~/site:/srv");
        let grid = NSGridView::gridViewWithNumberOfColumns_rows(2, 0, self.mtm);
        grid.setRowSpacing(8.0);
        grid.setColumnSpacing(10.0);
        for (title, view) in [("Image", &image), ("Name", &name), ("Ports", &ports), ("Environment", &env), ("Volumes", &volumes)] {
            let key = label(self.mtm, title, 13.0, unsafe { NSFontWeightRegular });
            let row: [&NSView; 2] = [&key, &**view];
            grid.addRowWithViews(&NSArray::from_slice(&row));
        }
        grid.columnAtIndex(0).setXPlacement(NSGridCellPlacement::Trailing);
        grid.setFrameSize(grid.fittingSize());

        let profile = self.st.borrow().profile.clone();
        let detail = problem.unwrap_or_else(|| format!("Starts in the background on profile “{profile}”. Separate lists with spaces."));
        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&ns("Run an image"));
        alert.setInformativeText(&ns(&detail));
        alert.addButtonWithTitle(&ns("Run"));
        alert.addButtonWithTitle(&ns("Cancel"));
        alert.setAccessoryView(Some(&grid));
        alert.window().setInitialFirstResponder(Some(if form.image.is_empty() { &image } else { &name }));
        let block = RcBlock::new(move |response: NSModalResponse| {
            if response != NSAlertFirstButtonReturn {
                return;
            }
            let text = |field: &NSTextField| field.stringValue().to_string();
            let form = RunForm {
                image: text(&image),
                name: text(&name),
                ports: text(&ports),
                env: text(&env),
                volumes: text(&volumes),
            };
            with_ctl(|ctl| ctl.run_form(form));
        });
        alert.beginSheetModalForWindow_completionHandler(&self.window, Some(&block));
    }

    fn run_form(&self, form: RunForm) {
        let home = env::var_os("HOME").map(PathBuf::from);
        let args = match run_args(&form, home.as_deref()) {
            Ok(args) => args,
            Err(problem) => {
                // After this sheet has gone, so the next one can attach.
                after_main(Duration::ZERO, move || {
                    with_ctl(|ctl| ctl.show_run_form(form, Some(problem)));
                });
                return;
            }
        };
        let unshared = home.map(|home| unshared_mounts(&form, &home)).unwrap_or_default();
        if unshared.is_empty() {
            return self.run_docker(move |docker| run_image(docker.socket(), &args).err());
        }
        let detail = format!(
            "{}\n\nColima shares only your home folder with its VM, so these come up empty.",
            unshared.join("\n")
        );
        after_main(Duration::ZERO, move || {
            with_ctl(|ctl| {
                ctl.confirm("This folder is outside your home folder", &detail, "Run", move |ctl| {
                    ctl.run_docker(move |docker| run_image(docker.socket(), &args).err());
                });
            });
        });
    }

    fn ask_pull(&self) {
        let field = NSTextField::initWithFrame(
            NSTextField::alloc(self.mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(300.0, 24.0)),
        );
        field.setPlaceholderString(Some(&ns("nginx:alpine")));
        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&ns("Pull an image"));
        let profile = self.st.borrow().profile.clone();
        alert.setInformativeText(&ns(&format!("Pulled into profile “{profile}”. Without a tag, “latest” is used.")));
        alert.addButtonWithTitle(&ns("Pull"));
        alert.addButtonWithTitle(&ns("Cancel"));
        alert.setAccessoryView(Some(&field));
        alert.window().setInitialFirstResponder(Some(&field));
        let input = field.clone();
        let block = RcBlock::new(move |response: NSModalResponse| {
            let reference = input.stringValue().to_string();
            let reference = reference.trim().to_string();
            if response == NSAlertFirstButtonReturn && !reference.is_empty() {
                with_ctl(|ctl| ctl.pull(reference));
            }
        });
        alert.beginSheetModalForWindow_completionHandler(&self.window, Some(&block));
    }

    fn pull(&self, reference: String) {
        self.st.borrow_mut().pull = Some(format!("starting {reference}"));
        self.render_chrome();
        let docker = self.docker();
        thread::spawn(move || {
            let result = docker.pull(&reference, |text| post(Msg::PullProgress(text)));
            post(Msg::PullDone(result));
        });
    }

    fn copy_selection(&self) {
        let keys = if self.st.borrow().page == Page::Containers {
            self.selected_ids()
        } else {
            let Some(keys) = self.current_table().map(Table::selected_keys) else { return };
            keys
        };
        let board = NSPasteboard::generalPasteboard();
        board.clearContents();
        board.setString_forType(&ns(&keys.join("\n")), unsafe { NSPasteboardTypeString });
    }

    /// Selects the project's header, which stands for all of its containers
    /// whatever the filters hide.
    fn select_project(&self) {
        let keys: Vec<String> = {
            let st = self.st.borrow();
            let selected = self.selected_ids();
            st.containers
                .iter()
                .filter(|c| selected.contains(&c.id) && !c.project().is_empty())
                .map(|c| pages::project_key(c.project()))
                .collect()
        };
        self.containers.select_keys(&keys);
    }

    // Sampling: nothing runs unless its numbers are on screen.

    fn toggle_stats(&self) {
        let on = !self.st.borrow().stats_on;
        self.st.borrow_mut().stats_on = on;
        let (icon, tip) = if on {
            ("chart.bar.fill", "Showing CPU and memory")
        } else {
            ("chart.bar", "Show CPU and memory")
        };
        self.toolbar.set_icon(Action::ToggleStats, icon, tip);
        self.sync_sampler();
    }

    /// Starts or stops the one sampler to match what is on screen.
    pub fn sync_sampler(&self) {
        let active = NSApplication::sharedApplication(self.mtm).isActive();
        let want = {
            let st = self.st.borrow();
            match st.page {
                Page::Containers if st.stats_on && st.engine_up && active => Some(SampleKind::Containers),
                Page::Colima if st.engine_up && active => Some(SampleKind::Vm),
                _ => None,
            }
        };
        if want != Some(SampleKind::Containers) {
            self.hud.set_stats(None);
        }
        if self.st.borrow().sampler.as_ref().map(|sampler| sampler.kind) == want {
            return;
        }
        if let Some(sampler) = self.st.borrow_mut().sampler.take() {
            sampler.stop.store(true, Ordering::Relaxed);
        }
        let Some(kind) = want else { return };
        let epoch = self.st.borrow().epoch;
        let stop = Arc::new(AtomicBool::new(false));
        let targets = Arc::new(Mutex::new(Vec::new()));
        self.st.borrow_mut().sampler = Some(Sampler { kind, stop: stop.clone(), targets: targets.clone() });
        match kind {
            SampleKind::Containers => {
                self.hud.set_stats(Some("Measuring…"));
                self.update_stats_targets();
                let docker = self.docker();
                thread::spawn(move || {
                    let mut meter = StatsMeter::default();
                    while !stop.load(Ordering::Relaxed) {
                        let ids = targets.lock().unwrap().clone();
                        let started = Instant::now();
                        let samples: Vec<(String, ContainerStats)> =
                            ids.iter().filter_map(|id| docker.stats(id).ok().map(|stats| (id.clone(), stats))).collect();
                        post(Msg::Stats(meter.read(&samples)));
                        let pause = STATS_PERIOD.max(started.elapsed() * STATS_REST_RATIO);
                        rest(pause, || stop.load(Ordering::Relaxed) || *targets.lock().unwrap() != ids);
                    }
                });
            }
            SampleKind::Vm => {
                let Some(colima) = self.colima() else { return };
                thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        if let Some(usage) = colima.vm_usage() {
                            post(Msg::VmUsage(epoch, usage));
                        }
                        rest(VM_MEMORY_PERIOD, || stop.load(Ordering::Relaxed));
                    }
                });
            }
        }
    }

    /// One reading of the VM's disk, so a nearly full one shows on every page. The
    /// Colima page keeps its own up to date, through the sampler.
    pub fn check_vm_disk(&self) {
        let active = NSApplication::sharedApplication(self.mtm).isActive();
        let epoch = {
            let mut st = self.st.borrow_mut();
            let fresh = st.vm_disk_read.is_some_and(|read| read.elapsed() < VM_DISK_PERIOD);
            if !st.engine_up || !active || fresh {
                return;
            }
            st.vm_disk_read = Some(Instant::now());
            st.epoch
        };
        let Some(colima) = self.colima() else { return };
        thread::spawn(move || {
            if let Some(usage) = colima.vm_usage() {
                post(Msg::VmUsage(epoch, usage));
            }
        });
    }

    fn sampling(&self, kind: SampleKind) -> bool {
        self.st.borrow().sampler.as_ref().is_some_and(|sampler| sampler.kind == kind)
    }

    /// The running containers being measured: the selected ones, or all of them
    /// when none of the selected ones is running.
    fn stats_targets(&self) -> Vec<String> {
        let st = self.st.borrow();
        let selected = self.selected_ids();
        let running = || st.containers.iter().filter(|c| c.is_running());
        let chosen: Vec<String> = running().filter(|c| selected.contains(&c.id)).map(|c| c.id.clone()).collect();
        if chosen.is_empty() { running().map(|c| c.id.clone()).collect() } else { chosen }
    }

    fn update_stats_targets(&self) {
        if self.sampling(SampleKind::Containers) {
            let ids = self.stats_targets();
            if let Some(sampler) = &self.st.borrow().sampler {
                *sampler.targets.lock().unwrap() = ids;
            }
        }
    }

    fn stats_label(&self) -> String {
        if let [project] = self.selected_projects().as_slice() {
            return project.clone();
        }
        let selected = self.selected_ids();
        let st = self.st.borrow();
        let chosen: Vec<&Container> = st.containers.iter().filter(|c| c.is_running() && selected.contains(&c.id)).collect();
        match chosen.as_slice() {
            [] => "All containers".to_string(),
            [only] => only.name().to_string(),
            many => format!("{} containers", many.len()),
        }
    }

    fn set_running_only(&self, on: bool) {
        self.st.borrow_mut().running_only = on;
        let (icon, tip) = if on {
            ("line.3.horizontal.decrease.circle.fill", "Showing running containers only")
        } else {
            ("line.3.horizontal.decrease.circle", "Show running containers only")
        };
        self.toolbar.set_icon(Action::RunningOnly, icon, tip);
    }

    // Details, terminal, ports and mounts: each runs only when asked for.

    fn show_details(&self) {
        let id = match self.selected_ids().as_slice() {
            [id] => id.clone(),
            _ => return,
        };
        let docker = self.docker();
        thread::spawn(move || post(Msg::Details(docker.inspect(&id).map(Box::new))));
    }

    fn details_arrived(&self, result: DResult<Box<ContainerDetails>>) {
        let details = match result {
            Ok(details) => details,
            Err(error) => return self.show_error("Could not read the container", &error),
        };
        let digests = {
            let st = self.st.borrow();
            st.images.iter().find(|i| i.id == details.image).map(|i| i.repo_digests.clone()).unwrap_or_default()
        };
        let text = pages::details_text(&pages::container_details(&details, &digests, now()));
        let (scroll, view) = console(self.mtm);
        view.setTextContainerInset(NSSize::new(6.0, 6.0));
        append_text(&view, &text);
        scroll.setBorderType(NSBorderType::BezelBorder);
        scroll.setFrameSize(NSSize::new(560.0, 280.0));

        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&ns(details.name.trim_start_matches('/')));
        alert.setInformativeText(&ns(&details.config.image));
        alert.addButtonWithTitle(&ns("Done"));
        alert.setAccessoryView(Some(&scroll));
        alert.beginSheetModalForWindow_completionHandler(&self.window, None);
    }

    /// Opens a shell in the selected container in Terminal. A .command file
    /// needs no Automation permission, unlike scripting Terminal.
    fn open_terminal(&self) {
        let target = {
            let st = self.st.borrow();
            match self.selected_ids().as_slice() {
                [id] => st.containers.iter().find(|c| c.id == *id && c.state == "running").map(|c| c.id.clone()),
                _ => None,
            }
        };
        let Some(id) = target else { return };
        let Some(cli) = find_tool("docker") else {
            let detail = "Open in Terminal runs docker exec. Install the CLI with: brew install docker";
            return self.show_error("The Docker CLI is not installed", detail);
        };
        let script = exec_script(&cli, self.docker().socket(), &id);
        // Unique per request: each script deletes itself, so a shared name would
        // pull the file out from under a second window on the same container.
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let path = env::temp_dir().join(format!("dockerplus-{}-{stamp}.command", short_id(&id)));
        let written = fs::write(&path, script).and_then(|()| fs::set_permissions(&path, fs::Permissions::from_mode(0o700)));
        let url = NSURL::fileURLWithPath(&ns(&path.to_string_lossy()));
        if written.is_err() || !NSWorkspace::sharedWorkspace().openURL(&url) {
            self.show_error("Could not open Terminal", &path.display().to_string());
        }
    }

    /// Fills the per-container submenus of the context menu as it opens.
    pub fn menu_will_open(&self, menu: &NSMenu) {
        let ports = menu.itemWithTag(Action::OpenPort as NSInteger);
        let mounts = menu.itemWithTag(Action::RevealMount as NSInteger);
        if ports.is_none() && mounts.is_none() {
            return;
        }
        let (links, folders) = {
            let st = self.st.borrow();
            match self.selected_ids().as_slice() {
                [id] => st
                    .containers
                    .iter()
                    .find(|c| c.id == *id)
                    .map_or_else(Default::default, |c| (pages::port_links(&c.ports), host_mounts(c))),
                _ => Default::default(),
            }
        };
        self.fill_submenu(ports.as_deref(), links);
        self.fill_submenu(mounts.as_deref(), folders);
    }

    fn fill_submenu(&self, item: Option<&NSMenuItem>, entries: Vec<(String, String)>) {
        let Some(item) = item else { return };
        if entries.is_empty() {
            // Without a submenu the item validates through can() and greys out.
            item.setSubmenu(None);
            unsafe { item.setAction(Some(sel!(dpPerform:))) };
            return;
        }
        let submenu = NSMenu::new(self.mtm);
        for (title, location) in entries {
            let entry = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(self.mtm),
                    &ns(&title),
                    Some(sel!(dpOpen:)),
                    &ns(""),
                )
            };
            unsafe {
                entry.setTarget(Some(&*self.actions));
                entry.setRepresentedObject(Some(&*ns(&location)));
            }
            submenu.addItem(&entry);
        }
        item.setSubmenu(Some(&submenu));
    }

    /// Opens a URL in the browser, or shows a host path in Finder. Paths are only
    /// ever revealed: opening one would launch it if the folder is an app bundle.
    pub fn open_location(&self, location: &str) {
        let workspace = NSWorkspace::sharedWorkspace();
        let opened = if location.starts_with("http://") || location.starts_with("https://") {
            NSURL::URLWithString(&ns(location)).is_some_and(|url| workspace.openURL(&url))
        } else {
            let url = NSURL::fileURLWithPath(&ns(location));
            workspace.activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
            true
        };
        if !opened {
            self.show_error("Could not open it", location);
        }
    }

    // Logs

    fn open_logs(&self, id: &str) {
        let name = {
            let st = self.st.borrow();
            st.containers.iter().find(|c| c.id == id).map(|c| c.name().to_string())
        };
        let Some(name) = name else { return };
        self.stop_logs();

        let cancel = Cancel::new();
        let generation = {
            let mut st = self.st.borrow_mut();
            st.log_generation += 1;
            st.logs = Some(LogSession {
                generation: st.log_generation,
                container: id.to_string(),
                cancel: cancel.clone(),
            });
            st.log_generation
        };
        self.logs.begin(&name);
        if self.logs.view.isHidden() {
            self.logs.view.setHidden(false);
            self.detail.adjustSubviews();
            let height = self.detail.frame().size.height;
            self.detail.setPosition_ofDividerAtIndex(height - LOG_PANEL_HEIGHT, 0);
        }

        let docker = self.docker();
        let id = id.to_string();
        thread::spawn(move || {
            let buffer = LogBuffer::new(generation);
            let mut tail = LOG_TAIL;
            let result = loop {
                let mut cleaner = LogCleaner::default();
                let result = docker.logs(&id, tail, true, &cancel, |raw| {
                    if buffer.push(&cleaner.feed(raw)) {
                        cancel.interrupt();
                    }
                });
                buffer.push(&cleaner.finish());
                if cancel.is_cancelled() || !buffer.take_behind() {
                    break result;
                }
                buffer.push(LOG_JUMP_NOTE);
                tail = LOG_JUMP_TAIL;
            };
            let error = if cancel.is_cancelled() { None } else { result.err() };
            post(Msg::LogEnded(generation, error));
        });
    }

    fn stop_logs(&self) {
        if let Some(session) = self.st.borrow_mut().logs.take() {
            session.cancel.cancel();
        }
    }

    fn close_logs(&self) {
        self.stop_logs();
        self.logs.clear();
        self.logs.view.setHidden(true);
        self.detail.adjustSubviews();
    }

    fn append_log(&self, generation: u64, data: &str) {
        let current = self.st.borrow().logs.as_ref().is_some_and(|s| s.generation == generation);
        if current {
            self.logs.append(data);
        }
    }

    fn log_ended(&self, generation: u64, error: Option<String>) {
        let current = self.st.borrow().logs.as_ref().is_some_and(|s| s.generation == generation);
        if current {
            let reason = error.map_or("(log stream ended)".to_string(), |e| format!("({e})"));
            self.logs.note(&format!("\n{reason}\n"));
        }
    }

    // Dialogs

    fn confirm(&self, title: &str, detail: &str, button: &str, on_ok: impl FnOnce(&Controller) + 'static) {
        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&ns(title));
        alert.setInformativeText(&ns(detail));
        let confirm = alert.addButtonWithTitle(&ns(button));
        confirm.setHasDestructiveAction(true);
        // These cannot be undone, so Return must not do it. Esc still cancels.
        confirm.setKeyEquivalent(&ns(""));
        alert.addButtonWithTitle(&ns("Cancel"));
        let on_ok = Cell::new(Some(on_ok));
        let block = RcBlock::new(move |response: NSModalResponse| {
            if response == NSAlertFirstButtonReturn {
                if let Some(on_ok) = on_ok.take() {
                    with_ctl(on_ok);
                }
            }
        });
        alert.beginSheetModalForWindow_completionHandler(&self.window, Some(&block));
    }

    /// Colima logs through a pipe to us, so quitting mid-start would kill it
    /// with SIGPIPE and could leave the VM half set up.
    pub fn quit_reply(&self) -> NSApplicationTerminateReply {
        if self.st.borrow().op.is_none() {
            return NSApplicationTerminateReply::TerminateNow;
        }
        self.window.makeKeyAndOrderFront(None);
        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&ns("Colima is still working"));
        alert.setInformativeText(&ns("Quitting now interrupts it and can leave the virtual machine half set up."));
        alert.addButtonWithTitle(&ns("Keep Running"));
        alert.addButtonWithTitle(&ns("Quit Anyway"));
        if alert.runModal() == NSAlertSecondButtonReturn {
            NSApplicationTerminateReply::TerminateNow
        } else {
            NSApplicationTerminateReply::TerminateCancel
        }
    }

    fn show_error(&self, title: &str, detail: &str) {
        self.show_alert(NSAlertStyle::Warning, title, detail);
    }

    fn show_notice(&self, title: &str, detail: &str) {
        self.show_alert(NSAlertStyle::Informational, title, detail);
    }

    fn show_alert(&self, style: NSAlertStyle, title: &str, detail: &str) {
        let alert = NSAlert::new(self.mtm);
        alert.setAlertStyle(style);
        alert.setMessageText(&ns(title));
        alert.setInformativeText(&ns(detail));
        if self.window.attachedSheet().is_some() {
            alert.runModal();
        } else {
            alert.beginSheetModalForWindow_completionHandler(&self.window, None);
        }
    }
}

fn running_note(running: usize) -> String {
    match running {
        0 => "No containers are running.".to_string(),
        n => format!("{n} running {} will stop.", pages::plural(n, "container")),
    }
}

fn choice_popup(mtm: MainThreadMarker, values: &[u32], unit: &str, selected: u32) -> Retained<NSPopUpButton> {
    let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    for value in values {
        popup.addItemWithTitle(&ns(&format!("{value}{unit}")));
    }
    if let Some(index) = values.iter().position(|v| *v == selected) {
        popup.selectItemAtIndex(index as NSInteger);
    }
    popup
}

/// What the New Profile sheet was filled in with, kept to bring it back when
/// the name will not do.
#[derive(Clone, Debug)]
struct NewProfile {
    name: String,
    size: Resources,
    /// Index into `VM_KINDS`.
    kind: usize,
    rosetta: bool,
    kubernetes: bool,
    problem: Option<String>,
}

impl Default for NewProfile {
    fn default() -> Self {
        Self { name: String::new(), size: Resources::DEFAULT, kind: 0, rosetta: false, kubernetes: false, problem: None }
    }
}

fn saved_profile() -> String {
    let saved = NSUserDefaults::standardUserDefaults().stringForKey(&ns(PROFILE_KEY));
    saved.map(|name| name.to_string()).filter(|name| !name.is_empty()).unwrap_or_else(|| DEFAULT_PROFILE.to_string())
}

fn checkbox(mtm: MainThreadMarker, title: &str, on: bool) -> Retained<NSButton> {
    let button = unsafe { NSButton::checkboxWithTitle_target_action(&ns(title), None, None, mtm) };
    button.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
    button
}

fn is_on(button: &NSButton) -> bool {
    button.state() == NSControlStateValueOn
}

/// Bind mounts the Mac can open. Colima shares the home folder at the same
/// path, so only sources inside it name the same place on both sides.
fn host_mounts(container: &Container) -> Vec<(String, String)> {
    let Some(home) = env::var_os("HOME").map(PathBuf::from) else { return Vec::new() };
    pages::bind_mounts(&container.mounts)
        .into_iter()
        .filter(|(_, path)| Path::new(path).starts_with(&home) && Path::new(path).exists())
        .collect()
}

/// Lets the main thread steer the engine watcher.
struct WatchControl {
    /// The open event stream, so it can be ended from outside.
    stream: Mutex<Option<Cancel>>,
    /// Bumped whenever the engine to watch changes.
    generation: Mutex<u64>,
    changed: Condvar,
}

static WATCH: WatchControl =
    WatchControl { stream: Mutex::new(None), generation: Mutex::new(0), changed: Condvar::new() };

impl WatchControl {
    fn generation(&self) -> u64 {
        *self.generation.lock().unwrap()
    }

    /// Ends the open event stream, or the one still connecting, and the watcher
    /// opens a fresh one.
    fn interrupt(&self) {
        if let Some(stream) = self.stream.lock().unwrap().as_ref() {
            stream.cancel();
        }
    }

    /// Makes the watcher start over: it drops its stream, cuts any pause short,
    /// and announces whatever the engine socket now says, up or down.
    fn restart(&self) {
        *self.generation.lock().unwrap() += 1;
        self.changed.notify_all();
        self.interrupt();
    }

    /// Waits `period`, or less if a restart comes first.
    fn pause(&self, period: Duration, seen: u64) {
        let generation = self.generation.lock().unwrap();
        let _ = self.changed.wait_timeout_while(generation, period, |now| *now == seen);
    }
}

/// The pause before reopening the event stream, and the one to use next time.
/// A stream that ran a while was healthy, so the pauses start over; streams that
/// keep ending at once wait twice as long each time, up to a ceiling.
fn retry_after(wait: Duration, lasted: Duration) -> (Duration, Duration) {
    let wait = if lasted >= EVENTS_HEALTHY { EVENTS_RETRY } else { wait };
    (wait, (wait * 2).min(EVENTS_RETRY_MAX))
}

/// Follows the engine: long-polls its event stream while it is up, and quietly
/// retries the socket while it is not. Idle cost is one blocked read, plus a ping
/// whenever the stream has been silent for a minute.
fn spawn_watcher(docker: Arc<Mutex<Docker>>) {
    thread::spawn(move || {
        let mut seen = WATCH.generation();
        let mut up = None;
        let mut wait = EVENTS_RETRY;
        loop {
            let generation = WATCH.generation();
            if generation != seen {
                (seen, up, wait) = (generation, None, EVENTS_RETRY);
            }
            let current = docker.lock().unwrap().clone();
            let answers = current.ping();
            // After a restart the engine is announced either way, so the main
            // thread hears about the one it now shows.
            if up != Some(answers) {
                up = Some(answers);
                post(Msg::Engine(seen, answers));
            }
            if !answers {
                WATCH.pause(ENGINE_RETRY, seen);
                continue;
            }
            let cancel = Cancel::new();
            *WATCH.stream.lock().unwrap() = Some(cancel.clone());
            // A restart since the ping would otherwise go unnoticed until this
            // stream ends; one from here on cancels it.
            if WATCH.generation() != seen {
                continue;
            }
            let opened = Instant::now();
            let _ = current.watch_events(&cancel, || current.ping(), |scope| post(Msg::Changed(scope)));
            *WATCH.stream.lock().unwrap() = None;
            let (pause, next) = retry_after(wait, opened.elapsed());
            wait = next;
            WATCH.pause(pause, seen);
            // Anything could have happened while the stream was down.
            post(Msg::Changed(Scope::ALL));
        }
    });
}

/// Collects log bytes off the main thread and hands them over in batches, so a
/// noisy container costs a few text updates a second rather than thousands.
struct LogBuffer {
    generation: u64,
    inner: Arc<Mutex<LogPending>>,
}

#[derive(Default)]
struct LogPending {
    data: String,
    scheduled: bool,
    /// Time the reader has spent waiting since it was last caught up.
    waited: Duration,
    behind: bool,
}

impl LogBuffer {
    fn new(generation: u64) -> Self {
        Self { generation, inner: Arc::default() }
    }

    /// Waits while the UI is behind, so the reader slows to the speed of the
    /// text view and Docker sends more slowly. Returns true once it has waited so
    /// long that the stream should be reopened at the end of the log.
    fn push(&self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let mut pending = self.inner.lock().unwrap();
        let mut waited = false;
        while pending.data.len() >= LOG_BUFFER_LIMIT {
            drop(pending);
            thread::sleep(LOG_BACKOFF);
            waited = true;
            pending = self.inner.lock().unwrap();
            pending.waited += LOG_BACKOFF;
        }
        if !waited && pending.data.is_empty() {
            pending.waited = Duration::ZERO;
        }
        if pending.waited >= LOG_MAX_BEHIND {
            pending.waited = Duration::ZERO;
            pending.behind = true;
        }
        let behind = pending.behind;
        pending.data.push_str(text);
        if pending.scheduled {
            return behind;
        }
        pending.scheduled = true;
        let inner = self.inner.clone();
        let generation = self.generation;
        after_main(LOG_FLUSH, move || {
            let data = {
                let mut pending = inner.lock().unwrap();
                pending.scheduled = false;
                std::mem::take(&mut pending.data)
            };
            with_ctl(|ctl| ctl.append_log(generation, &data));
        });
        behind
    }

    fn take_behind(&self) -> bool {
        std::mem::take(&mut self.inner.lock().unwrap().behind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colima_rechecks_thin_out_to_once_a_minute() {
        let waits: Vec<u64> = (0..8).map(|tries| recheck_delay(tries).as_secs()).collect();
        assert_eq!(waits, [3, 6, 12, 24, 48, 60, 60, 60]);
        assert_eq!(recheck_delay(u32::MAX), COLIMA_RECHECK_LONGEST);
    }

    #[test]
    fn event_stream_retries_back_off_and_start_over() {
        let quick = Duration::from_millis(5);
        assert_eq!(retry_after(EVENTS_RETRY, quick), (EVENTS_RETRY, EVENTS_RETRY * 2));
        assert_eq!(retry_after(EVENTS_RETRY * 2, quick).0, EVENTS_RETRY * 2);
        let mut wait = EVENTS_RETRY;
        for _ in 0..20 {
            wait = retry_after(wait, quick).1;
        }
        assert_eq!(wait, EVENTS_RETRY_MAX);
        assert_eq!(retry_after(wait, EVENTS_HEALTHY), (EVENTS_RETRY, EVENTS_RETRY * 2));
    }
}
