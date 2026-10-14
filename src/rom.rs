//! `Rom`: the runomine handle of the current request, shared by the tower/axum and actix-web integrations.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::client::Client;
use crate::event::{PageView, RequestInfo, Track};
use crate::hash::today;

/// What the middleware stores in the request's extensions.
#[derive(Clone)]
pub(crate) struct RomExt {
    pub client: Client,
    pub ctx: Arc<RomContext>,
}

pub(crate) struct RomContext {
    pub info: RequestInfo,
    /// Set with [`Rom::identify`]; read by the middleware for the pageview.
    pub user: Mutex<Option<String>>,
}

impl RomContext {
    pub fn new(info: RequestInfo) -> Self {
        RomContext {
            info,
            user: Mutex::new(None),
        }
    }

    pub fn user(&self) -> Option<String> {
        let identified = self.user.lock().unwrap_or_else(|e| e.into_inner()).clone();
        identified.or_else(|| self.info.user.clone())
    }

    /// Records this request's pageview.
    pub fn record_pageview(&self, client: &Client) {
        let mut pageview = PageView::from(self.info.clone());
        pageview.user = self.user();
        client.pageview(pageview);
    }
}

/// The current request's runomine handle: an axum or actix-web extractor that never fails.
///
/// `rom.track(…)` records an event with this request's visitor hash, URL, user agent and country attached.
/// With the integration's middleware installed it uses that middleware's client and IP settings; without it, the
/// process-wide [`crate::client()`] and the TCP peer.
#[derive(Clone)]
pub struct Rom {
    pub(crate) client: Client,
    pub(crate) ctx: Arc<RomContext>,
}

impl fmt::Debug for Rom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rom")
            .field("url", &self.ctx.info.url)
            .field("user", &self.ctx.user())
            .finish_non_exhaustive()
    }
}

impl Rom {
    pub(crate) fn from_ext(ext: &RomExt) -> Self {
        Rom {
            client: ext.client.clone(),
            ctx: Arc::clone(&ext.ctx),
        }
    }

    /// Records a business event in this request: `rom.track("signup", Track::new().prop("plan", "free"))`.
    /// The request (and a user set with [`Rom::identify`]) is attached unless `details` sets its own.
    pub fn track(&self, name: impl AsRef<str>, details: Track) {
        if !self.client.is_enabled() {
            return;
        }
        let mut details = details;
        if details.request.is_none() {
            details.request = Some(self.ctx.info.clone());
        }
        if details.user.is_none() {
            details.user = self.ctx.user();
        }
        self.client.track(name, details);
    }

    /// The visitor hash of this request (today, with the client's token), `None` when the client is disabled.
    /// Store it with an order at checkout and pass it to [`Track::visitor`] in the payment webhook.
    pub fn visitor(&self) -> Option<String> {
        // A disabled client didn't read the request: no hash rather than one of empty fields.
        if !self.client.is_enabled() {
            return None;
        }
        let info = &self.ctx.info;
        let host = info
            .host
            .clone()
            .unwrap_or_else(|| crate::util::host_of_url(info.url.as_deref().unwrap_or("")));
        self.client.core().visitor(
            info.ip.as_deref().unwrap_or(""),
            info.user_agent.as_deref().unwrap_or(""),
            &host,
            &today(),
        )
    }

    /// What to pass to your payment provider when creating a checkout (Stripe `metadata`, Paddle `custom_data`, …),
    /// so runomine can attribute the sale: `runomine_visitor` (this request's visitor hash) and `runomine_user`
    /// (the user set with [`Rom::identify`], else the middleware's). Only the keys with a value: no visitor when
    /// the client is disabled or the request has no IP.
    pub fn checkout_metadata(&self) -> BTreeMap<String, String> {
        self.client
            .core()
            .checkout_metadata(&self.ctx.info, self.ctx.user(), &today())
    }

    /// Your user id for this request (e.g. right after login or signup): this request's pageview and later
    /// `rom.track` calls carry it.
    pub fn identify(&self, user: impl fmt::Display) {
        let user = user.to_string();
        if !user.is_empty() {
            *self.ctx.user.lock().unwrap_or_else(|e| e.into_inner()) = Some(user);
        }
    }

    /// What was taken from the request (the IP included: it stays on your server).
    pub fn request(&self) -> &RequestInfo {
        &self.ctx.info
    }

    pub fn client(&self) -> &Client {
        &self.client
    }
}
