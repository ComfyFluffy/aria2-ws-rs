use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, Weak};

use crate::response::Event;
use crate::{error, Client, Error, Hooks, InnerClient, RpcRequest, RpcResponse, TaskHooks};
use futures::future::BoxFuture;
use futures::prelude::*;
use futures::stream::{SplitSink, SplitStream};
use futures::{StreamExt, TryStreamExt};
use log::warn;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use snafu::prelude::*;
use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::select;
use tokio::sync::{broadcast, Notify};
use tokio::time::sleep;
use tokio::{
    spawn,
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::response::Notification;

macro_rules! try_continue {
    ($res:expr) => {
        match $res {
            Ok(v) => v,
            Err(err) => {
                warn!("{}", err);
                continue;
            }
        }
    };
}

async fn on_reconnect(inner_client: Weak<InnerClient>) -> Result<(), Error> {
    if let Some(client) = inner_client.upgrade() {
        let mut res: HashMap<String, Map<String, Value>> = HashMap::new();
        for mut map in client
            .custom_tell_stopped(
                0,
                1000,
                Some(
                    ["status", "totalLength", "completedLength", "gid"]
                        .into_iter()
                        .map(|x| x.to_string())
                        .collect(),
                ),
            )
            .await?
        {
            let gid = if let Some(Value::String(s)) = map.remove("gid") {
                s
            } else {
                Err(Error::ReconnectHook {
                    message: format!("unexpected response: {:?}", map),
                })?
            };
            res.insert(gid, map);
        }

        let mut lock = client.hooks.lock().unwrap();
        for (gid, hooks) in &mut lock.0 {
            if let Some(status) = res.get(gid) {
                let total = status.get("totalLength");
                if total.is_some() && total == status.get("completedLength") {
                    if let Some(h) = hooks.on_complete.take() {
                        spawn(h);
                    }
                }
                if let Some(Value::String(s)) = status.get("status") {
                    if s == "error" {
                        if let Some(h) = hooks.on_error.take() {
                            spawn(h);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn match_hook(event: Event, hook: &mut TaskHooks) -> Option<BoxFuture<'static, ()>> {
    use Event::*;
    match event {
        // "aria2.onDownloadStart" => &mut hook.on_start,
        // "aria2.onDownloadPause" => &mut hook.on_pause,
        // "aria2.onDownloadStop" => &mut hook.on_stop,
        Complete => &mut hook.on_complete,
        Error => &mut hook.on_error,
        BtComplete => &mut hook.on_complete,
        _ => return None,
    }
    .take()
}

fn process_nofitications(notification: &Notification, hooks: &Hooks) -> Result<(), Error> {
    use Event::*;
    if !matches!(notification.event, Complete | Error | BtComplete) {
        return Ok(());
    }
    let mut lock = hooks.lock().unwrap();
    if let Some(hook) = lock.0.get_mut(&notification.gid) {
        if let Some(hook) = match_hook(notification.event, hook) {
            spawn(hook);
        }
    } else {
        match lock.1.entry(notification.gid.clone()) {
            Entry::Occupied(mut e) => {
                e.get_mut().insert(notification.event);
            }
            Entry::Vacant(e) => {
                let mut set = HashSet::with_capacity(1);
                set.insert(notification.event);
                e.insert(set);
            }
        }
    }
    Ok(())
}

fn get_gid_from_notifictaion(req: &RpcRequest) -> Option<&str> {
    req.params.get(0)?.get("gid")?.as_str()
}

impl InnerClient {
    fn id(&self) -> u64 {
        self.id.fetch_add(1, SeqCst)
    }

    fn subscribe_id<T>(
        &self,
        id: u64,
        timeout: Option<Duration>,
    ) -> BoxFuture<'static, Result<T, Error>>
    where
        T: DeserializeOwned + Send,
    {
        let (tx, rx) = oneshot::channel();
        self.subscribes.lock().unwrap().insert(id, tx);
        let timeout = timeout.unwrap_or(self.default_timeout);
        let subscribes = self.subscribes.clone();

        async move {
            let res = if timeout.is_zero() {
                rx.await.context(error::OneshotRecvSnafu)?
            } else {
                match tokio::time::timeout(timeout, rx).await {
                    Ok(r) => r.context(error::OneshotRecvSnafu)?,
                    Err(e) => {
                        subscribes.lock().unwrap().remove(&id);
                        Err(Error::Timeout { source: e })?
                    }
                }
            };

            if let Some(err) = res.error {
                return Err(Error::Aria2 { source: err });
            }

            if let Some(v) = res.result {
                Ok(serde_json::from_value::<T>(v).context(error::JsonSnafu)?)
            } else {
                error::ResultNotFoundSnafu { response: res }.fail()
            }
        }
        .boxed()
    }

    async fn call(&self, id: u64, method: &str, mut params: Vec<Value>) -> Result<(), Error> {
        if let Some(ref token) = self.token {
            params.insert(0, Value::String(token.clone()))
        }
        let req = RpcRequest {
            id: Some(id),
            jsonrpc: "2.0".to_string(),
            method: "aria2.".to_string() + method,
            params,
        };
        self.tx_write
            .send(Message::Text(
                serde_json::to_string(&req).context(error::JsonSnafu)?,
            ))
            .await
            .context(error::MpscSendMessageSnafu)?;
        Ok(())
    }

    pub async fn call_and_subscribe<T>(
        &self,
        method: &str,
        params: Vec<Value>,
        timeout: Option<Duration>,
    ) -> Result<T, Error>
    where
        T: DeserializeOwned + Send,
    {
        let id = self.id();
        let fut = self.subscribe_id::<T>(id, timeout);
        self.call(id, method, params).await?;
        fut.await
    }
}

async fn read_worker(
    mut read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    subscribes: Arc<Mutex<HashMap<u64, oneshot::Sender<RpcResponse>>>>,
    hooks: Hooks,
    tx_not: broadcast::Sender<Notification>,
) -> Result<(), Error> {
    while let Some(msg) = read.try_next().await.context(error::WebsocketSnafu)? {
        let s = try_continue!(msg.to_text());
        let v = try_continue!(serde_json::from_str::<Value>(s));
        if let Value::Object(obj) = &v {
            if obj.contains_key("method") {
                // The message is a notification.
                // https://aria2.github.io/manual/en/html/aria2c.html#notifications
                let errf = || error::UnexpectedMessageSnafu { msg: msg.clone() };
                let req: RpcRequest =
                    try_continue!(serde_json::from_value(v).context(error::JsonSnafu));
                let gid = try_continue!(get_gid_from_notifictaion(&req).with_context(errf));
                let not = try_continue!(
                    Notification::new(gid.to_string(), &req.method).with_context(errf)
                );

                try_continue!(process_nofitications(&not, &hooks));

                let _ = tx_not.send(not);
                continue;
            }
        }

        let res: RpcResponse = try_continue!(serde_json::from_value(v).context(error::JsonSnafu));
        if let Some(ref id) = res.id {
            let tx = subscribes.lock().unwrap().remove(id);
            if let Some(tx) = tx {
                let _ = tx.send(res);
            }
        }
    }
    Ok(())
}

async fn write_worker(
    mut write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    mut rx_write: mpsc::Receiver<Message>,
    exit: Arc<Notify>,
) -> mpsc::Receiver<Message> {
    loop {
        select! {
            msg = rx_write.recv() => {
                if let Some(msg) = msg {
                    try_continue!(write.send(msg).await);
                } else {
                    return rx_write;
                }
            },
            _ = exit.notified() => {
                return rx_write;
            }
        }
    }
}

impl Client {
    /// Create a new `Client` and connect to the given url.
    ///
    /// # Example
    ///
    /// ```
    /// use aria2_ws::Client;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let client = Client::connect("ws://127.0.0.1:6800/jsonrpc", None)
    ///         .await
    ///         .unwrap();
    ///     let gid = client
    ///         .add_uri(
    ///             vec!["https://go.dev/dl/go1.17.6.windows-amd64.msi".to_string()],
    ///             None,
    ///             None,
    ///             None,
    ///         )
    ///         .await
    ///         .unwrap();
    ///     client.force_remove(gid).await.unwrap();
    /// }
    /// ```
    pub async fn connect(url: &str, token: Option<&str>) -> Result<Self, Error> {
        let (tx_write, mut rx_write) = mpsc::channel::<Message>(4);
        let subscribes: Arc<Mutex<HashMap<u64, oneshot::Sender<RpcResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let hooks: Hooks = Arc::new(Mutex::new((HashMap::new(), HashMap::new())));
        let shutdown = Arc::new(Notify::new());
        let url = url.to_string();
        let (tx_not, _) = broadcast::channel(16);

        let inner = Arc::new(InnerClient {
            tx_write,
            id: AtomicU64::new(0),
            token: token.map(|t| "token:".to_string() + t),
            subscribes: subscribes.clone(),
            shutdown: shutdown.clone(),
            hooks: hooks.clone(),
            default_timeout: Duration::from_secs(10),
            extendet_timeout: Duration::from_secs(120),
            tx_not: tx_not.clone(),
        });

        let inner_weak = Arc::downgrade(&inner);
        spawn(async move {
            let mut reconnect = false;
            loop {
                let connected = select! {
                    r = tokio_tungstenite::connect_async(&url) => r,
                    _ = shutdown.notified() => return,
                };
                match connected {
                    Ok((ws, _)) => {
                        let (write, read) = ws.split();
                        if reconnect {
                            spawn(on_reconnect(inner_weak.clone()));
                        } else {
                            reconnect = true;
                        }
                        let read_fut =
                            read_worker(read, subscribes.clone(), hooks.clone(), tx_not.clone());

                        let exit = Arc::new(Notify::new());
                        let write_fut = spawn(write_worker(write, rx_write, exit.clone()));

                        select! {
                            result = read_fut => {
                                warn!("aria2 disconnected: {:?}", result);
                                exit.notify_waiters();
                                rx_write = write_fut.await.unwrap();
                            },
                            _ = shutdown.notified() => {
                                exit.notify_waiters();
                                return;
                            }
                        }
                    }
                    Err(err) => {
                        warn!("aria2: connect failed: {}", err);
                        sleep(Duration::from_secs(3)).await;
                    }
                }
            }
        });

        Ok(Self(inner))
    }

    pub async fn call(&self, id: u64, method: &str, params: Vec<Value>) -> Result<(), Error> {
        self.0.call(id, method, params).await
    }

    pub async fn call_and_subscribe<T: DeserializeOwned + Send>(
        &self,
        method: &str,
        params: Vec<Value>,
        timeout: Option<Duration>,
    ) -> Result<T, Error> {
        self.0.call_and_subscribe(method, params, timeout).await
    }

    pub async fn set_hooks(&self, gid: &str, hooks: Option<TaskHooks>) {
        if let Some(mut hook) = hooks {
            let gid = gid.to_string();
            let mut lock = self.0.hooks.lock().unwrap();
            if let Some(set) = lock.1.get_mut(&gid) {
                let set = std::mem::take(set);
                for event in set {
                    if let Some(h) = match_hook(event, &mut hook) {
                        spawn(h);
                    }
                }
            }
            lock.0.insert(gid, hook);
        }
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Notification> {
        self.0.tx_not.subscribe()
    }
}
