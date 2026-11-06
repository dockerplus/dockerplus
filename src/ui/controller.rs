//! Owns the app state. Background threads never touch it: they send a [`Msg`]
//! to the main thread, and [`Controller::render`] brings the views in line.

use super::actions::{Action, Actions};
use super::colima_page::ColimaPage;
use super::hud::{EmptyState, HUD_CLEARANCE, Hud};
use super::logs::LogPanel;
use super::pages::{self, Page};
use super::sidebar::{Sidebar, StatusView};
use super::table::{Table, filter_rows};
use super::toolbar::Toolbar;
use super::util::*;
use crate::backend::colima::{Colima, ColimaInfo, ResourceChange, Resources, find_tool};
use crate::backend::docker::{ContainerAction, DResult, Docker, Snapshot, exec_script};
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
use std::collections::HashMap;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PROFILE: &str = "default";
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
const LOG_JUMP_NOTE: &[u8] = b"\n--- too much output to show it all: skipped ahead to the latest lines ---\n\n";
/// Coalesces bursts of engine events, such as a `compose up`, into one refresh.
const REFRESH_DEBOUNCE: Duration = Duration::from_millis(120);
/// Keeps "Up 3 minutes" and relative dates from going stale without any events.
const TICK: Duration = Duration::from_secs(30);
const ENGINE_RETRY: Duration = Duration::from_secs(2);
/// How often to re-ask Colima while it says "running" but Docker is silent.
const COLIMA_RECHECK: Duration = Duration::from_secs(3);

pub enum Msg {
    Colima(Box<ColimaInfo>),
    Engine(bool),
    Changed,
    Snapshot(DResult<Snapshot>),
    Version(Option<EngineVersion>),
    VolumeSizes(HashMap<String, i64>),
    CliLine(String),
    CliDone(Result<(), String>),
    ActionDone(Option<String>),
    PullProgress(String),
    PullDone(Result<(), String>),
    LogEnded(u64, Option<String>),
    Details(DResult<Box<ContainerDetails>>),
}

thread_local! {
    static CTL: OnceCell<Rc<Controller>> = const { OnceCell::new() };
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

struct LogSession {
    generation: u64,
    container: String,
    cancel: Cancel,
}

#[derive(Default)]
pub struct State {
    pub page: Page,
    navigated: bool,
    pub info: Option<ColimaInfo>,
    pub engine_up: bool,
    engine: Option<EngineVersion>,
    op: Option<ColimaOp>,
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub volumes: Vec<Volume>,
    sizes: HashMap<String, i64>,
    filter: String,
    refreshing: bool,
    refresh_again: bool,
    refresh_scheduled: bool,
    recheck_scheduled: bool,
    busy_actions: usize,
    pull: Option<String>,
    logs: Option<LogSession>,
    log_generation: u64,
    pub running_only: bool,
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
    let images = table(pages::IMAGE_COLUMNS, &[Some(("Copy ID", Action::CopyId)), None, Some(("Remove…", Action::Remove))]);
    let volumes = table(pages::VOLUME_COLUMNS, &[Some(("Copy Name", Action::CopyId)), None, Some(("Remove…", Action::Remove))]);

    let colima_page = ColimaPage::new(mtm, &actions);
    let hud = Hud::new(mtm);
    let empty = EmptyState::new(mtm, &actions);
    let logs = LogPanel::new(mtm, &actions);

    let pages_view = NSView::new(mtm);
    for view in [&*containers.scroll as &NSView, &images.scroll, &volumes.scroll, &colima_page.view] {
        pages_view.addSubview(view);
        pin(view, &pages_view);
    }
    pages_view.addSubview(&empty.view);
    pages_view.addSubview(&hud.view);
    empty.view.setTranslatesAutoresizingMaskIntoConstraints(false);
    hud.view.setTranslatesAutoresizingMaskIntoConstraints(false);
    activate(&[
        empty.view.centerXAnchor().constraintEqualToAnchor(&pages_view.centerXAnchor()),
        empty.view.centerYAnchor().constraintEqualToAnchor(&pages_view.centerYAnchor()),
        hud.view.centerXAnchor().constraintEqualToAnchor(&pages_view.centerXAnchor()),
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

    let docker = Docker::new(Colima::default_socket(PROFILE));
    let ctl = Rc::new(Controller {
        mtm,
        window: window.clone(),
        actions,
        sidebar,
        toolbar,
        containers,
        images,
        volumes,
        colima_page,
        logs,
        hud,
        empty,
        detail,
        _split: split,
        colima: Colima::locate(PROFILE),
        docker: Arc::new(Mutex::new(docker)),
        st: RefCell::default(),
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
        self.tick();
    }

    fn docker(&self) -> Docker {
        self.docker.lock().unwrap().clone()
    }

    fn phase(&self) -> Phase {
        let st = self.st.borrow();
        match st.op {
            Some(ColimaOp::Stop) => return Phase::Stopping,
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
            Msg::Colima(info) => {
                if let Some(socket) = info.status.as_ref().map(|s| s.docker_socket.as_str()) {
                    self.adopt_socket(socket);
                }
                self.st.borrow_mut().info = Some(*info);
                self.render();
                self.recheck_if_unsettled();
            }
            Msg::Engine(up) => self.engine_changed(up),
            Msg::Changed => self.schedule_refresh(),
            Msg::Snapshot(result) => self.snapshot_arrived(result),
            Msg::Version(version) => {
                self.st.borrow_mut().engine = version;
                self.render();
            }
            Msg::VolumeSizes(sizes) => {
                self.st.borrow_mut().sizes = sizes;
                self.render();
            }
            Msg::CliLine(line) => self.colima_page.log_line(&line),
            Msg::CliDone(result) => {
                self.st.borrow_mut().op = None;
                if let Err(error) = result {
                    self.colima_page.log_line(&format!("Failed: {error}"));
                    self.show_error("Colima could not finish", &error);
                }
                self.reload_colima();
                self.render();
            }
            Msg::ActionDone(error) => {
                {
                    let mut st = self.st.borrow_mut();
                    st.busy_actions = st.busy_actions.saturating_sub(1);
                }
                if let Some(error) = error {
                    self.show_error("Docker refused that", &error);
                }
                self.refresh();
                self.render();
            }
            Msg::PullProgress(text) => {
                self.st.borrow_mut().pull = Some(text);
                self.render();
            }
            Msg::PullDone(result) => {
                self.st.borrow_mut().pull = None;
                if let Err(error) = result {
                    self.show_error("Could not pull the image", &error);
                }
                self.refresh();
                self.render();
            }
            Msg::LogEnded(generation, error) => self.log_ended(generation, error),
            Msg::Details(result) => self.details_arrived(result),
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
        let Some(colima) = self.colima.clone() else {
            self.st.borrow_mut().info = Some(ColimaInfo::default());
            self.render();
            return;
        };
        thread::spawn(move || post(Msg::Colima(Box::new(colima.info()))));
    }

    /// Nothing announces a VM going down when its socket has already gone, and
    /// Colima keeps listing it as running for a few seconds. Keep asking until
    /// it says stopped or Docker answers, so the UI cannot stick on "Waiting".
    fn recheck_if_unsettled(&self) {
        {
            let mut st = self.st.borrow_mut();
            let running = st.info.as_ref().is_some_and(|info| info.running);
            if !running || st.engine_up || st.op.is_some() || st.recheck_scheduled {
                return;
            }
            st.recheck_scheduled = true;
        }
        after_main(COLIMA_RECHECK, || {
            with_ctl(|ctl| {
                ctl.st.borrow_mut().recheck_scheduled = false;
                ctl.reload_colima();
            });
        });
    }

    fn engine_changed(&self, up: bool) {
        {
            let mut st = self.st.borrow_mut();
            st.engine_up = up;
            if !up {
                st.containers.clear();
                st.images.clear();
                st.volumes.clear();
                st.sizes.clear();
                st.engine = None;
            }
        }
        if up {
            self.refresh();
            let docker = self.docker();
            thread::spawn(move || post(Msg::Version(docker.version().ok())));
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
        self.render();
    }

    /// One refresh at a time; anything arriving meanwhile queues exactly one more.
    fn refresh(&self) {
        {
            let mut st = self.st.borrow_mut();
            if !st.engine_up {
                return;
            }
            if st.refreshing {
                st.refresh_again = true;
                return;
            }
            st.refreshing = true;
        }
        let docker = self.docker();
        thread::spawn(move || post(Msg::Snapshot(docker.snapshot())));
    }

    fn schedule_refresh(&self) {
        {
            let mut st = self.st.borrow_mut();
            if st.refresh_scheduled {
                return;
            }
            st.refresh_scheduled = true;
        }
        after_main(REFRESH_DEBOUNCE, || {
            with_ctl(|ctl| {
                ctl.st.borrow_mut().refresh_scheduled = false;
                ctl.refresh();
            });
        });
    }

    fn snapshot_arrived(&self, result: DResult<Snapshot>) {
        let (again, volumes_changed) = {
            let mut st = self.st.borrow_mut();
            st.refreshing = false;
            let again = std::mem::take(&mut st.refresh_again);
            let mut volumes_changed = false;
            if let Ok(snapshot) = result {
                volumes_changed = st.volumes != snapshot.volumes;
                st.containers = snapshot.containers;
                st.images = snapshot.images;
                st.volumes = snapshot.volumes;
            }
            (again, volumes_changed)
        };
        self.render();
        if again {
            self.refresh();
        }
        if volumes_changed {
            self.load_volume_sizes();
        }
    }

    fn load_volume_sizes(&self) {
        if self.st.borrow().page != Page::Volumes {
            return;
        }
        let docker = self.docker();
        thread::spawn(move || {
            if let Ok(sizes) = docker.volume_sizes() {
                post(Msg::VolumeSizes(sizes));
            }
        });
    }

    fn tick(&self) {
        after_main(TICK, || {
            with_ctl(|ctl| {
                if NSApplication::sharedApplication(ctl.mtm).isActive() {
                    ctl.refresh();
                }
                ctl.tick();
            });
        });
    }

    // Rendering

    pub fn render(&self) {
        self.render_tables();
        self.render_status();
        self.render_page();
        self.render_toolbar();
    }

    fn render_tables(&self) {
        let (containers, images, volumes) = {
            let st = self.st.borrow();
            let now = now();
            let query = st.filter.as_str();
            (
                filter_rows(
                    pages::container_rows(st.containers.iter().filter(|c| !st.running_only || c.is_running()), now),
                    query,
                ),
                filter_rows(pages::image_rows(&st.images, &st.containers, now), query),
                filter_rows(pages::volume_rows(&st.volumes, &st.containers, &st.sizes, now), query),
            )
        };
        self.containers.set_rows(containers);
        self.images.set_rows(images);
        self.volumes.set_rows(volumes);

        let st = self.st.borrow();
        let up = st.engine_up;
        self.sidebar.set_count(Page::Containers, up.then_some(st.containers.len()));
        self.sidebar.set_count(Page::Images, up.then_some(st.images.len()));
        self.sidebar.set_count(Page::Volumes, up.then_some(st.volumes.len()));
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

        let version = if info.version.is_empty() { String::new() } else { format!("Colima {}", info.version) };
        let headline = match phase {
            Phase::NotInstalled => "Colima is not installed. Run: brew install colima".to_string(),
            Phase::Stopped if !info.provisioned => "No virtual machine yet. The first start downloads one.".to_string(),
            _ => format!("Profile “{PROFILE}”{}", if version.is_empty() { String::new() } else { format!(" · {version}") }),
        };
        self.colima_page.update(&info, st.engine.as_ref(), &headline, title);

        let idle = st.op.is_none();
        let installed = self.colima.is_some();
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
        self.empty.view.setHidden(page == Page::Colima || up);
        self.hud.view.setHidden(page == Page::Colima || !up);

        let summary = match page {
            Page::Colima => String::new(),
            Page::Containers => pages::containers_summary(&st.containers),
            Page::Images => pages::images_summary(&st.images),
            Page::Volumes => pages::volumes_summary(&st.volumes),
        };
        self.window.setTitle(&ns(page.title()));
        self.window.setSubtitle(&ns(if up { &summary } else { "" }));
        self.hud.set_summary(&summary);

        let activity = st.pull.as_ref().map(|text| format!("Pull: {text}")).or_else(|| {
            (st.busy_actions > 0).then(|| "Working…".to_string())
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
            Action::Pull,
            Action::Remove,
            Action::RunningOnly,
            Action::Refresh,
        ] {
            let visible = match action {
                Action::ContainerStart
                | Action::ContainerStop
                | Action::ContainerRestart
                | Action::ShowLogs
                | Action::RunningOnly => containers,
                Action::Pull => page == Page::Images,
                Action::Remove => page != Page::Colima,
                _ => true,
            };
            self.toolbar.set(action, visible && up, self.can(action));
        }
        self.toolbar.set_search_visible(page != Page::Colima && up);
    }

    /// Whether an action applies right now. Drives both toolbar and menus.
    pub fn can(&self, action: Action) -> bool {
        let phase = self.phase();
        let st = self.st.borrow();
        let selected: Vec<&Container> = self
            .containers
            .selected_keys()
            .iter()
            .filter_map(|key| st.containers.iter().find(|c| c.id == *key))
            .collect();
        match action {
            Action::ColimaStart => phase == Phase::Stopped,
            Action::ColimaStop | Action::ColimaRestart => matches!(phase, Phase::Running | Phase::NoEngine),
            Action::ContainerStart => selected.iter().any(|c| !c.is_running()),
            Action::ContainerStop => selected.iter().any(|c| c.is_running()),
            Action::ContainerRestart => !selected.is_empty(),
            Action::ShowLogs => selected.len() == 1,
            // Not while half-started: the sizes Colima reports then are unreliable.
            Action::ColimaResources => self.colima.is_some() && matches!(phase, Phase::Stopped | Phase::Running),
            Action::ContainerPause => selected.iter().any(|c| c.state == "running"),
            Action::ContainerResume => selected.iter().any(|c| c.state == "paused"),
            Action::ShowDetails => st.page == Page::Containers && selected.len() == 1,
            Action::OpenTerminal => matches!(selected.as_slice(), [c] if c.state == "running"),
            Action::OpenPort => matches!(selected.as_slice(), [c] if !pages::port_links(&c.ports).is_empty()),
            Action::RevealMount => matches!(selected.as_slice(), [c] if !host_mounts(c).is_empty()),
            Action::SelectProject => selected.iter().any(|c| !c.project().is_empty()),
            Action::RunningOnly => st.engine_up,
            Action::Pull | Action::Refresh => st.engine_up,
            Action::Remove | Action::CopyId => match st.page {
                Page::Colima => false,
                Page::Containers => !selected.is_empty(),
                _ => !self.current_table().is_some_and(|t| t.selected_keys().is_empty()),
            },
            _ => true,
        }
    }

    fn current_table(&self) -> Option<&Table> {
        match self.st.borrow().page {
            Page::Colima => None,
            Page::Containers => Some(&self.containers),
            Page::Images => Some(&self.images),
            Page::Volumes => Some(&self.volumes),
        }
    }

    // User input

    pub fn show_page(&self, page: Page) {
        #[cfg(feature = "devtools")]
        self.note_page_change(page);
        self.st.borrow_mut().page = page;
        if page == Page::Volumes {
            self.load_volume_sizes();
        }
        self.render();
    }

    pub fn selection_changed(&self) {
        self.render_toolbar();
        // With the panel open, the logs follow the selection.
        let target = {
            let st = self.st.borrow();
            if st.page != Page::Containers || st.logs.is_none() {
                return;
            }
            let keys = self.containers.selected_keys();
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
                    if let [id] = self.containers.selected_keys().as_slice() {
                        self.open_logs(id);
                    }
                }
            }
            Action::Pull => self.ask_pull(),
            Action::Refresh => {
                self.refresh();
                self.reload_colima();
                self.load_volume_sizes();
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
            Action::LogsFollow => self.logs.toggle_follow(),
            Action::LogsClear => self.logs.clear(),
            Action::LogsClose => self.close_logs(),
            Action::ColimaResources => self.ask_resources(),
            Action::ContainerPause => self.container_action(ContainerAction::Pause),
            Action::ContainerResume => self.container_action(ContainerAction::Unpause),
            Action::ShowDetails => self.show_details(),
            Action::OpenTerminal => self.open_terminal(),
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
        let running = self.st.borrow().containers.iter().filter(|c| c.is_running()).count();
        let (verb, button) = if op == ColimaOp::Stop { ("Stop", "Stop") } else { ("Restart", "Restart") };
        let detail = running_note(running);
        self.confirm(&format!("{verb} Colima?"), &detail, button, move |ctl| ctl.colima_op(op));
    }

    fn colima_op(&self, op: ColimaOp) {
        let Some(colima) = self.colima.clone() else { return };
        {
            let mut st = self.st.borrow_mut();
            if st.op.is_some() {
                return;
            }
            st.op = Some(op);
        }
        clear_text(&self.colima_page.console);
        self.render();
        let on_line = |line| post(Msg::CliLine(line));
        thread::spawn(move || {
            let result = match op {
                ColimaOp::Start => colima.start(ResourceChange::default(), on_line),
                ColimaOp::Stop => colima.stop(on_line),
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

    /// Asks for a new VM size. Colima only applies it on start, so an up VM is
    /// restarted, with the same warning as a plain restart.
    fn ask_resources(&self) {
        let up = self.phase() == Phase::Running;
        let (current, running) = {
            let st = self.st.borrow();
            let current = st.info.as_ref().and_then(Resources::current);
            (current, st.containers.iter().filter(|c| c.is_running()).count())
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
        for (title, popup) in [("CPUs", &cpus), ("Memory", &memory), ("Disk", &disk)] {
            let key = label(self.mtm, title, 13.0, unsafe { NSFontWeightRegular });
            let row: [&NSView; 2] = [&key, popup];
            grid.addRowWithViews(&NSArray::from_slice(&row));
        }
        grid.columnAtIndex(0).setXPlacement(NSGridCellPlacement::Trailing);
        grid.setFrameSize(grid.fittingSize());

        let detail = if up {
            format!("Colima restarts to apply this. {} Disk size can only grow.", running_note(running))
        } else {
            "Colima starts with these settings. Disk size can only grow.".to_string()
        };
        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&ns("Colima resources"));
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

    // Containers, images and volumes

    fn container_action(&self, action: ContainerAction) {
        let keys = self.containers.selected_keys();
        let targets: Vec<String> = {
            let st = self.st.borrow();
            let applies = |c: &Container| match action {
                ContainerAction::Start => !c.is_running(),
                ContainerAction::Stop => c.is_running(),
                ContainerAction::Restart => true,
                ContainerAction::Pause => c.state == "running",
                ContainerAction::Unpause => c.state == "paused",
            };
            keys.into_iter()
                .filter(|key| st.containers.iter().any(|c| c.id == *key && applies(c)))
                .collect()
        };
        if targets.is_empty() {
            return;
        }
        self.run_docker(move |docker| {
            targets.iter().find_map(|id| docker.container_action(id, action).err())
        });
    }

    /// Runs engine work off the main thread, tracked by the activity capsule.
    fn run_docker(&self, work: impl FnOnce(&Docker) -> Option<String> + Send + 'static) {
        self.st.borrow_mut().busy_actions += 1;
        self.render();
        let docker = self.docker();
        thread::spawn(move || post(Msg::ActionDone(work(&docker))));
    }

    fn confirm_remove(&self) {
        let page = self.st.borrow().page;
        let Some(table) = self.current_table() else { return };
        let keys = table.selected_keys();
        if keys.is_empty() {
            return;
        }
        let count = keys.len();
        let names: Vec<String> = {
            let st = self.st.borrow();
            keys.iter()
                .map(|key| match page {
                    Page::Containers => st
                        .containers
                        .iter()
                        .find(|c| c.id == *key)
                        .map_or(key.clone(), |c| c.name().to_string()),
                    _ => key.clone(),
                })
                .take(5)
                .collect()
        };
        let any_running = {
            let st = self.st.borrow();
            page == Page::Containers
                && keys.iter().any(|key| st.containers.iter().any(|c| c.id == *key && c.is_running()))
        };
        let noun = match page {
            Page::Containers => "container",
            Page::Images => "image",
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
            ctl.run_docker(move |docker| {
                keys.iter().find_map(|key| {
                    match page {
                        Page::Containers => docker.remove_container(key, true),
                        Page::Images => docker.remove_image(key),
                        _ => docker.remove_volume(key),
                    }
                    .err()
                })
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
        alert.setInformativeText(&ns("Pulled into Colima's Docker engine. Without a tag, “latest” is used."));
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
        self.render();
        let docker = self.docker();
        thread::spawn(move || {
            let result = docker.pull(&reference, |text| post(Msg::PullProgress(text)));
            post(Msg::PullDone(result));
        });
    }

    fn copy_selection(&self) {
        let Some(keys) = self.current_table().map(Table::selected_keys) else { return };
        let board = NSPasteboard::generalPasteboard();
        board.clearContents();
        board.setString_forType(&ns(&keys.join("\n")), unsafe { NSPasteboardTypeString });
    }

    /// Selects every container in the Compose projects of the selected ones, so
    /// the usual actions apply to the whole project. Filters are cleared when
    /// they hide part of it, or Remove would quietly miss those containers.
    fn select_project(&self) {
        let keys: Vec<String> = {
            let st = self.st.borrow();
            let selected = self.containers.selected_keys();
            let projects: Vec<&str> = st
                .containers
                .iter()
                .filter(|c| selected.contains(&c.id) && !c.project().is_empty())
                .map(Container::project)
                .collect();
            st.containers.iter().filter(|c| projects.contains(&c.project())).map(|c| c.id.clone()).collect()
        };
        if self.containers.select_keys(&keys) < keys.len() {
            self.st.borrow_mut().filter.clear();
            self.toolbar.set_search_text("");
            self.set_running_only(false);
            self.render();
            self.containers.select_keys(&keys);
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
        let id = match self.containers.selected_keys().as_slice() {
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
            match self.containers.selected_keys().as_slice() {
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
            match self.containers.selected_keys().as_slice() {
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
                let result = docker.logs(&id, tail, true, &cancel, |text| {
                    if buffer.push(text) {
                        cancel.interrupt();
                    }
                });
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

    fn append_log(&self, generation: u64, data: &[u8]) {
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
        alert.addButtonWithTitle(&ns(button)).setHasDestructiveAction(true);
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
        let alert = NSAlert::new(self.mtm);
        alert.setAlertStyle(NSAlertStyle::Warning);
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

/// Bind mounts the Mac can open. Colima shares the home folder at the same
/// path, so only sources inside it name the same place on both sides.
fn host_mounts(container: &Container) -> Vec<(String, String)> {
    let Some(home) = env::var_os("HOME").map(PathBuf::from) else { return Vec::new() };
    pages::bind_mounts(&container.mounts)
        .into_iter()
        .filter(|(_, path)| Path::new(path).starts_with(&home) && Path::new(path).exists())
        .collect()
}

/// Follows the engine: long-polls its event stream while it is up, and quietly
/// retries the socket while it is not. Idle cost is one blocked read.
fn spawn_watcher(docker: Arc<Mutex<Docker>>) {
    thread::spawn(move || {
        let mut up = false;
        loop {
            let current = docker.lock().unwrap().clone();
            if current.ping() {
                if !up {
                    up = true;
                    post(Msg::Engine(true));
                }
                let _ = current.watch_events(&Cancel::new(), || post(Msg::Changed));
                thread::sleep(Duration::from_millis(300));
            } else {
                if up {
                    up = false;
                    post(Msg::Engine(false));
                }
                thread::sleep(ENGINE_RETRY);
            }
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
    data: Vec<u8>,
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
    fn push(&self, text: &[u8]) -> bool {
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
        pending.data.extend_from_slice(text);
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
