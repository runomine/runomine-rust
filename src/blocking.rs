//! A client without an async runtime: a bounded queue drained by one background std thread that sends with
//! [ureq](https://docs.rs/ureq). For CLIs, workers, and sync frameworks.
//!
//! ```no_run
//! let rom = runomine::blocking::Client::from_env();
//! rom.track("export_finished", runomine::Track::new().user("42").prop("rows", 1200));
//! rom.shutdown(); // before the process exits
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::core::{Core, Step, USER_AGENT};
use crate::event::{PageView, RequestInfo, Track, WireEvent};
use crate::hash::today;

enum Msg {
    Event(Box<WireEvent>),
    Flush(SyncSender<()>),
    Shutdown(SyncSender<()>),
}

/// Same API as the async `runomine::Client`, with blocking `flush` and `shutdown`. Cheap to clone.
///
/// The background thread is started with the first event; a disabled client starts nothing. Don't call
/// [`Client::flush`] or [`Client::shutdown`] from inside an async runtime's worker thread: they block.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    core: Arc<Core>,
    tx: OnceLock<Option<SyncSender<Msg>>>,
    closed: AtomicBool,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("blocking::Client")
            .field("enabled", &self.is_enabled())
            .field("endpoint", &self.inner.core.endpoint)
            .finish()
    }
}

impl Client {
    pub fn new(config: Config) -> Self {
        Client {
            inner: Arc::new(Inner {
                core: Arc::new(Core::new(config)),
                tx: OnceLock::new(),
                closed: AtomicBool::new(false),
            }),
        }
    }

    /// From `RUNOMINE_TOKEN`, `RUNOMINE_URL`, `RUNOMINE_ENABLED` and `RUNOMINE_DEBUG`.
    pub fn from_env() -> Self {
        Self::new(Config::from_env())
    }

    pub fn disabled() -> Self {
        Self::new(Config {
            enabled: false,
            ..Config::default()
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.core.active && !self.inner.closed.load(Ordering::Relaxed)
    }

    /// Records a pageview. Returns immediately.
    pub fn pageview(&self, pageview: PageView) {
        if !self.is_enabled() {
            return;
        }
        if let Some(event) = self.inner.core.pageview(pageview) {
            self.enqueue(event);
        }
    }

    /// Records a business event. Returns immediately.
    pub fn track(&self, name: impl AsRef<str>, details: Track) {
        if !self.is_enabled() {
            return;
        }
        if let Some(event) = self.inner.core.track(name.as_ref(), details) {
            self.enqueue(event);
        }
    }

    /// Today's visitor hash for this site, `None` without a token.
    pub fn visitor(&self, ip: &str, user_agent: &str, host: &str) -> Option<String> {
        self.inner.core.visitor(ip, user_agent, host, &today())
    }

    /// What to pass to your payment provider when creating a checkout: `runomine_visitor` (the request's visitor
    /// hash) and `runomine_user` (`request.user`), only the keys with a value. See the async client's
    /// `checkout_metadata`.
    pub fn checkout_metadata(&self, request: &RequestInfo) -> BTreeMap<String, String> {
        self.inner.core.checkout_metadata(request, None, &today())
    }

    /// Sends everything queued and waits for it. Never fails.
    pub fn flush(&self) {
        if let Some(Some(tx)) = self.inner.tx.get() {
            let (ack, done) = mpsc::sync_channel(1);
            if tx.send(Msg::Flush(ack)).is_ok() {
                let _ = done.recv();
            }
        }
    }

    /// Flushes and stops the background thread; later events are ignored. Call it before the process exits.
    pub fn shutdown(&self) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(Some(tx)) = self.inner.tx.get() {
            let (ack, done) = mpsc::sync_channel(1);
            if tx.send(Msg::Shutdown(ack)).is_ok() {
                let _ = done.recv();
            }
        }
    }

    #[doc(hidden)]
    pub fn worker_started(&self) -> bool {
        matches!(self.inner.tx.get(), Some(Some(_)))
    }

    fn enqueue(&self, event: WireEvent) {
        let core = &self.inner.core;
        let tx = self.inner.tx.get_or_init(|| {
            let (tx, rx) = mpsc::sync_channel(core.max_queue);
            let worker_core = Arc::clone(core);
            match std::thread::Builder::new()
                .name("runomine".into())
                .spawn(move || worker(&worker_core, rx))
            {
                Ok(_) => Some(tx),
                Err(err) => {
                    core.log(format_args!(
                        "could not start the background thread, runomine is off: {err}"
                    ));
                    None
                }
            }
        });
        let Some(tx) = tx else {
            return;
        };
        match tx.try_send(Msg::Event(Box::new(event))) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => core.log("queue full, event dropped"),
            Err(TrySendError::Disconnected(_)) => {
                core.log("background thread stopped, event dropped")
            }
        }
    }
}

fn agent(core: &Core) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(core.timeout))
        .http_status_as_error(false)
        .user_agent(USER_AGENT);
    #[cfg(all(feature = "native-tls", not(feature = "rustls")))]
    let config = config.tls_config(
        ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .build(),
    );
    config.build().into()
}

fn worker(core: &Core, rx: Receiver<Msg>) {
    let agent = agent(core);
    let mut buf: Vec<WireEvent> = Vec::new();
    let mut deadline: Option<Instant> = None;
    loop {
        let msg = match deadline {
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(at) => rx.recv_timeout(at.saturating_duration_since(Instant::now())),
        };
        match msg {
            Ok(Msg::Event(event)) => {
                buf.push(*event);
                deadline.get_or_insert_with(|| Instant::now() + core.flush_interval);
                if buf.len() >= core.batch_size {
                    send_all(&agent, core, &mut buf);
                    deadline = None;
                }
            }
            Ok(Msg::Flush(ack)) => {
                send_all(&agent, core, &mut buf);
                deadline = None;
                let _ = ack.send(());
            }
            Ok(Msg::Shutdown(ack)) => {
                let mut acks = vec![ack];
                while let Ok(msg) = rx.try_recv() {
                    match msg {
                        Msg::Event(event) => buf.push(*event),
                        Msg::Flush(ack) | Msg::Shutdown(ack) => acks.push(ack),
                    }
                }
                send_all(&agent, core, &mut buf);
                for ack in acks {
                    let _ = ack.send(());
                }
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                send_all(&agent, core, &mut buf);
                deadline = None;
            }
            Err(RecvTimeoutError::Disconnected) => {
                send_all(&agent, core, &mut buf);
                return;
            }
        }
    }
}

fn send_all(agent: &ureq::Agent, core: &Core, buf: &mut Vec<WireEvent>) {
    for chunk in buf.chunks(core.batch_size) {
        send(agent, core, chunk);
    }
    buf.clear();
}

fn send(agent: &ureq::Agent, core: &Core, batch: &[WireEvent]) {
    let Some(body) = core.body(batch) else {
        return;
    };
    for attempt in 0.. {
        let result = agent
            .post(&core.endpoint)
            .header("authorization", &format!("Bearer {}", core.token))
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .send(&body[..]);
        let result = match result {
            Ok(mut res) => {
                let status = res.status().as_u16();
                let retry_after = res
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                let text = res.body_mut().read_to_string().unwrap_or_default();
                Ok((status, retry_after, text))
            }
            Err(err) => Err(err.to_string()),
        };
        match core.judge(result, attempt, batch.len()) {
            Step::Done | Step::Drop => return,
            Step::Retry(wait) => std::thread::sleep(wait.max(Duration::from_millis(1))),
        }
    }
}
