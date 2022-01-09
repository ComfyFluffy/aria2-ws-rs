mod method;
pub mod response;
mod utils;

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, Weak};

use std::time::Duration;

use futures::future::BoxFuture;
use futures::prelude::*;
use futures::stream::{SplitSink, SplitStream};
use futures::{StreamExt, TryStreamExt};
use response::Event;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::atomic::Ordering::SeqCst;
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

use utils::serde_option_from_str;

use crate::response::Notification;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RpcRequest {
    pub id: Option<u64>,
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Vec<Value>,
}

/// Error returned by RPC calls.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Aria2Error {
    pub code: i32,
    pub message: String,
}
impl std::fmt::Display for Aria2Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "aria2 responsed error: code {}: {}",
            self.code, self.message
        )
    }
}
impl std::error::Error for Aria2Error {}

#[derive(Deserialize, Debug, Clone)]
pub struct RpcResponse {
    pub id: Option<u64>,
    pub jsonrpc: String,
    pub result: Option<Value>,
    pub error: Option<Aria2Error>,
}

/// Options of aria2 download tasks.
///
/// Add items to `extra_options` field to add custom options.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub struct TaskOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<Vec<String>>,
    #[serde(
        with = "serde_option_from_str",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub split: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub all_proxy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub out: Option<String>,
    #[serde(
        with = "serde_option_from_str",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub r#continue: Option<bool>,
    #[serde(
        with = "serde_option_from_str",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub auto_file_renaming: Option<bool>,
    #[serde(flatten)]
    pub extra_options: Map<String, Value>,
}

macro_rules! set_self_some {
    ($t:ident) => {
        pub fn $t(mut self, $t: String) -> Self {
            self.$t = Some($t);
            self
        }
    };
    ($t:ident, $ty:ty) => {
        pub fn $t(mut self, $t: $ty) -> Self {
            self.$t = Some($t);
            self
        }
    };
}

impl TaskOptions {
    pub fn new() -> Self {
        Self::default()
    }

    set_self_some!(header, Vec<String>);
    set_self_some!(split, u32);
    set_self_some!(all_proxy);
    set_self_some!(out);
    set_self_some!(dir);
}

/// Hooks that will be executed on notifications.
///
/// If the connection lost, all hooks will be checked whether they need to be executed once reconnected.
#[derive(Default)]
pub struct TaskHooks {
    // pub on_start: Option<BoxFuture<'static, ()>>,
    // pub on_pause: Option<BoxFuture<'static, ()>>,
    // pub on_stop: Option<BoxFuture<'static, ()>>,
    pub on_complete: Option<BoxFuture<'static, ()>>,
    pub on_error: Option<BoxFuture<'static, ()>>,
    // pub on_bt_complete: Option<BoxFuture<'static, ()>>,
}

type Hooks = Arc<Mutex<(HashMap<String, TaskHooks>, HashMap<String, HashSet<Event>>)>>;

struct InnerClient {
    token: Option<String>,
    tx_write: mpsc::Sender<Message>,
    id: AtomicU64,
    subscribes: Arc<Mutex<HashMap<u64, oneshot::Sender<RpcResponse>>>>,
    shutdown: Arc<Notify>,
    // hooks and pending events
    hooks: Hooks,
    default_timeout: Duration,
    extendet_timeout: Duration,
    tx_not: broadcast::Sender<response::Notification>,
}

/// An aria2 websocket rpc client.
///
/// The client contains an `Arc<InnerClient>)` and can be cloned.
#[derive(Clone)]
pub struct Client(Arc<InnerClient>);

impl Drop for InnerClient {
    fn drop(&mut self) {
        self.shutdown.notify_waiters();
    }
}

type Error = Box<dyn std::error::Error + Send + Sync>;

macro_rules! try_continue {
    ($res:expr, $msg:expr) => {
        match $res {
            Ok(v) => v,
            Err(err) => {
                println!("aria2: unexpected message: {}: {}", err, $msg);
                continue;
            }
        }
    };

    ($res:expr) => {
        match $res {
            Ok(v) => v,
            Err(err) => {
                println!("aria2 error: {}", err);
                continue;
            }
        }
    };
}

async fn on_reconnect(inner_client: &Weak<InnerClient>) -> Result<(), Error> {
    if let Some(client) = inner_client.upgrade() {
        let err = "aria2: unexpected response";
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
                Err(err)?
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

fn get_gid_from_notifictaion(req: &RpcRequest) -> Result<&str, Error> {
    let err = "aria2: unexpected notification";
    let gid = req
        .params
        .get(0)
        .ok_or(err)?
        .get("gid")
        .ok_or(err)?
        .as_str()
        .ok_or(err)?;
    Ok(gid)
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

        async move {
            let res = if timeout.is_zero() {
                rx.await?
            } else {
                tokio::time::timeout(timeout, rx).await??
            };

            if let Some(err) = res.error {
                Err(err)?;
            }

            let v = res.result.ok_or("aria2: result not found")?;
            Ok(serde_json::from_value::<T>(v)?)
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
            .send(Message::Text(serde_json::to_string(&req)?))
            .await?;
        Ok(())
    }

    async fn call_and_subscribe<T>(
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
    while let Some(msg) = read.try_next().await? {
        let s = try_continue!(msg.to_text());
        let v = try_continue!(serde_json::from_str::<Value>(s), s);
        if let Value::Object(obj) = &v {
            if obj.contains_key("method") {
                // The message is a notification.
                // https://aria2.github.io/manual/en/html/aria2c.html#notifications
                let req: RpcRequest = try_continue!(serde_json::from_value(v), s);
                let gid = try_continue!(get_gid_from_notifictaion(&req), s);
                let not = try_continue!(
                    Notification::new(gid.to_string(), &req.method)
                        .ok_or("unexpected notificaiton"),
                    s
                );

                try_continue!(process_nofitications(&not, &hooks), s);

                let _ = tx_not.send(not);
                continue;
            }
        }

        let res: RpcResponse = try_continue!(serde_json::from_value(v), s);
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
                    try_continue!(write.flush().await);
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
    /// ```rust
    /// use aria2_ws::Client;
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

        {
            let inner_weak = Arc::downgrade(&inner);
            spawn(async move {
                let mut reconnect = false;
                loop {
                    match tokio_tungstenite::connect_async(&url).await {
                        Ok((ws, _)) => {
                            let (write, read) = ws.split();
                            if reconnect {
                                let _ = on_reconnect(&inner_weak).await;
                            } else {
                                reconnect = true;
                            }
                            let read_fut = read_worker(
                                read,
                                subscribes.clone(),
                                hooks.clone(),
                                tx_not.clone(),
                            );

                            let exit = Arc::new(Notify::new());
                            let write_fut = spawn(write_worker(write, rx_write, exit.clone()));

                            select! {
                                result = read_fut => {
                                    println!("aria2 disconnected: {:?}", result);
                                    exit.notify_waiters();
                                    rx_write = write_fut.await.unwrap();
                                },
                                _ = shutdown.notified() => {
                                    return;
                                }
                            }
                        }
                        Err(err) => {
                            println!("aria2: connect failed: {}", err);
                            sleep(Duration::from_secs(3)).await;
                        }
                    }
                }
            });
        }

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

    async fn set_hooks(&self, gid: &str, hooks: Option<TaskHooks>) {
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
