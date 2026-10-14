//! The async client: a bounded in-memory queue drained by one background tokio task.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use crate::core::{Core, Step, USER_AGENT};
use crate::event::{PageView, RequestInfo, Track, WireEvent};
use crate::hash::today;

enum Msg {
    Event(Box<WireEvent>),
    Flush(oneshot::Sender<()>),
    Shutdown(oneshot::Sender<()>),
}

/// Sends pageviews and events to runomine in the background, in batches.
///
/// Cheap to clone (an `Arc` inside): keep one per site and share it. Recording never blocks and never fails: the
/// event is put on a bounded in-memory queue that a background task sends every 2 seconds or as soon as 500 are
/// waiting. The task starts with the first event, on a small runtime of its own (one `runomine` thread per
/// client), so it doesn't depend on your runtime's lifetime; a disabled client (no token, or
/// `RUNOMINE_ENABLED=false`) starts nothing and does nothing.
///
/// Call [`Client::shutdown`] before the process exits, or the last 2 seconds of events are lost.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    core: Arc<Core>,
    tx: OnceLock<Option<mpsc::Sender<Msg>>>,
    closed: AtomicBool,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("enabled", &self.is_enabled())
            .field("endpoint", &self.inner.core.endpoint)
            .finish()
    }
}

impl Client {
    /// A client with this configuration. Never fails: a config without a token gives a disabled client.
    pub fn new(config: Config) -> Self {
        Client {
            inner: Arc::new(Inner {
                core: Arc::new(Core::new(config)),
                tx: OnceLock::new(),
                closed: AtomicBool::new(false),
            }),
        }
    }

    /// A client configured from `RUNOMINE_TOKEN`, `RUNOMINE_URL`, `RUNOMINE_ENABLED` and `RUNOMINE_DEBUG`.
    pub fn from_env() -> Self {
        Self::new(Config::from_env())
    }

    /// A client that does nothing (tests, development).
    pub fn disabled() -> Self {
        Self::new(Config {
            enabled: false,
            ..Config::default()
        })
    }

    /// False when there is no token or the client was disabled: every call is then a no-op.
    pub fn is_enabled(&self) -> bool {
        self.inner.core.active && !self.inner.closed.load(Ordering::Relaxed)
    }

    /// Records a pageview. Returns immediately; the event goes out with the next batch.
    pub fn pageview(&self, pageview: PageView) {
        if !self.is_enabled() {
            return;
        }
        if let Some(event) = self.inner.core.pageview(pageview) {
            self.enqueue(event);
        }
    }

    /// Records a business event: `track("signup", Track::new().user(id))`. Returns immediately.
    ///
    /// Without [`Track::request`] or [`Track::visitor`] the event carries no `visitor`, and runomine attributes it
    /// through `user`. Inside a request, prefer the framework's `Rom` handle, which attaches the request.
    pub fn track(&self, name: impl AsRef<str>, details: Track) {
        if !self.is_enabled() {
            return;
        }
        if let Some(event) = self.inner.core.track(name.as_ref(), details) {
            self.enqueue(event);
        }
    }

    /// Today's visitor hash for this site: `None` when the client has no token. Store it with an order at
    /// checkout to attribute a later webhook's purchase to the visit ([`Track::visitor`]).
    pub fn visitor(&self, ip: &str, user_agent: &str, host: &str) -> Option<String> {
        self.inner.core.visitor(ip, user_agent, host, &today())
    }

    /// What to pass to your payment provider when creating a checkout (Stripe `metadata`, Paddle `custom_data`, …),
    /// so runomine can attribute the sale: `runomine_visitor` (the request's visitor hash, today) and
    /// `runomine_user` (`request.user`). Only the keys with a value: no visitor when the client is disabled or the
    /// request has no IP. Inside an axum/actix-web handler, prefer [`crate::Rom::checkout_metadata`].
    pub fn checkout_metadata(&self, request: &RequestInfo) -> BTreeMap<String, String> {
        self.inner.core.checkout_metadata(request, None, &today())
    }

    /// Sends everything queued so far and waits for it (or for runomine to be given up on). Never fails.
    pub async fn flush(&self) {
        let Some(Some(tx)) = self.inner.tx.get() else {
            return;
        };
        let (ack, done) = oneshot::channel();
        if tx.send(Msg::Flush(ack)).await.is_ok() {
            let _ = done.await;
        }
    }

    /// Flushes and stops the background task; later events are ignored. Await it after your server's graceful
    /// shutdown. A batch is retried for at most a few seconds (`timeout` × attempts + `retry_delays`), so wrap it
    /// in `tokio::time::timeout` if your platform gives you less.
    pub async fn shutdown(&self) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(Some(tx)) = self.inner.tx.get() else {
            return;
        };
        let (ack, done) = oneshot::channel();
        if tx.send(Msg::Shutdown(ack)).await.is_ok() {
            let _ = done.await;
        }
    }

    /// Whether the background task was started (tests).
    #[doc(hidden)]
    pub fn worker_started(&self) -> bool {
        matches!(self.inner.tx.get(), Some(Some(_)))
    }

    #[cfg(any(feature = "tower", feature = "actix-web"))]
    pub(crate) fn core(&self) -> &Core {
        &self.inner.core
    }

    fn enqueue(&self, event: WireEvent) {
        let core = &self.inner.core;
        let Some(tx) = self.inner.tx.get_or_init(|| start_worker(core)) else {
            return;
        };
        match tx.try_send(Msg::Event(Box::new(event))) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => core.log("queue full, event dropped"),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                core.log("background task stopped, event dropped")
            }
        }
    }
}

/// Starts the sender: a tokio task on its own current-thread runtime, in a thread named `runomine`. It doesn't
/// live on the app's runtime, so it outlives runtimes that stop before the app calls `shutdown` (actix-web's
/// workers, `#[tokio::test]`s), and events recorded outside any runtime are sent too.
fn start_worker(core: &Arc<Core>) -> Option<mpsc::Sender<Msg>> {
    let (tx, rx) = mpsc::channel(core.max_queue);
    let worker_core = Arc::clone(core);
    let spawned = std::thread::Builder::new()
        .name("runomine".into())
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt.block_on(worker(worker_core, rx)),
                Err(err) => worker_core.log(format_args!(
                    "could not start the sender, runomine is off: {err}"
                )),
            }
        });
    match spawned {
        Ok(_) => Some(tx),
        Err(err) => {
            core.log(format_args!(
                "could not start the sender thread, runomine is off: {err}"
            ));
            None
        }
    }
}

static GLOBAL: OnceLock<Client> = OnceLock::new();

/// The process-wide client, configured from the environment on first use unless [`init`] set it before.
/// The integrations use it when you don't pass a client.
pub fn client() -> &'static Client {
    GLOBAL.get_or_init(Client::from_env)
}

/// Sets the process-wide client returned by [`client`]. Returns false (and keeps the existing one) if it was
/// already set or used.
pub fn init(client: Client) -> bool {
    GLOBAL.set(client).is_ok()
}

async fn worker(core: Arc<Core>, mut rx: mpsc::Receiver<Msg>) {
    let builder = reqwest::Client::builder()
        .timeout(core.timeout)
        .user_agent(USER_AGENT);
    #[cfg(all(feature = "native-tls", not(feature = "rustls")))]
    let builder = builder.tls_backend_native();
    let http = match builder.build() {
        Ok(http) => http,
        Err(err) => {
            core.log(format_args!(
                "could not build the HTTP client, runomine is off: {err}"
            ));
            return;
        }
    };
    let mut buf: Vec<WireEvent> = Vec::new();
    // Set when the first event of a batch arrives: the batch goes out `flush_interval` later at the latest.
    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        let next = match deadline {
            None => Some(rx.recv().await),
            Some(at) => tokio::time::timeout_at(at, rx.recv()).await.ok(),
        };
        match next {
            // The interval elapsed.
            None => {
                send_all(&http, &core, &mut buf).await;
                deadline = None;
            }
            Some(Some(Msg::Event(event))) => {
                buf.push(*event);
                deadline.get_or_insert_with(|| tokio::time::Instant::now() + core.flush_interval);
                if buf.len() >= core.batch_size {
                    send_all(&http, &core, &mut buf).await;
                    deadline = None;
                }
            }
            Some(Some(Msg::Flush(ack))) => {
                send_all(&http, &core, &mut buf).await;
                deadline = None;
                let _ = ack.send(());
            }
            Some(Some(Msg::Shutdown(ack))) => {
                rx.close();
                let mut acks = vec![ack];
                while let Ok(msg) = rx.try_recv() {
                    match msg {
                        Msg::Event(event) => buf.push(*event),
                        Msg::Flush(ack) | Msg::Shutdown(ack) => acks.push(ack),
                    }
                }
                send_all(&http, &core, &mut buf).await;
                for ack in acks {
                    let _ = ack.send(());
                }
                return;
            }
            // Every client handle is gone.
            Some(None) => {
                send_all(&http, &core, &mut buf).await;
                return;
            }
        }
    }
}

async fn send_all(http: &reqwest::Client, core: &Core, buf: &mut Vec<WireEvent>) {
    for chunk in buf.chunks(core.batch_size) {
        send(http, core, chunk).await;
    }
    buf.clear();
}

async fn send(http: &reqwest::Client, core: &Core, batch: &[WireEvent]) {
    let Some(body) = core.body(batch) else {
        return;
    };
    for attempt in 0.. {
        let result = http
            .post(&core.endpoint)
            .header("authorization", format!("Bearer {}", core.token))
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .body(body.clone())
            .send()
            .await;
        let result = match result {
            Ok(res) => {
                let status = res.status().as_u16();
                let retry_after = res
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                let text = res.text().await.unwrap_or_default();
                Ok((status, retry_after, text))
            }
            Err(err) => Err(err.to_string()),
        };
        match core.judge(result, attempt, batch.len()) {
            Step::Done | Step::Drop => return,
            Step::Retry(wait) => tokio::time::sleep(wait.max(Duration::from_millis(1))).await,
        }
    }
}
