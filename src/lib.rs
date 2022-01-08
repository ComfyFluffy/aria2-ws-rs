mod method;
pub mod response;
mod utils;

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use std::time::Duration;

use futures::future::BoxFuture;
use futures::prelude::*;
use futures::stream::SplitStream;
use futures::{StreamExt, TryStreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::atomic::Ordering::SeqCst;
use tokio::net::TcpStream;
use tokio::select;
use tokio::sync::Notify;
use tokio::time::sleep;
use tokio::{
    spawn,
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use utils::serde_option_from_str;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RpcRequest {
    pub id: Option<u64>,
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Vec<Value>,
}

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

#[derive(Default)]
pub struct TaskHooks {
    // pub on_start: Option<BoxFuture<'static, ()>>,
    // pub on_pause: Option<BoxFuture<'static, ()>>,
    // pub on_stop: Option<BoxFuture<'static, ()>>,
    pub on_complete: Option<BoxFuture<'static, ()>>,
    pub on_error: Option<BoxFuture<'static, ()>>,
    pub on_bt_complete: Option<BoxFuture<'static, ()>>,
}

type Hooks = Arc<Mutex<(HashMap<String, TaskHooks>, HashMap<String, HashSet<String>>)>>;

pub struct Client {
    token: Option<String>,
    tx_write: mpsc::Sender<Message>,
    id: AtomicU64,
    subscribes: Arc<Mutex<HashMap<u64, oneshot::Sender<RpcResponse>>>>,
    shutdown: Arc<Notify>,
    // hooks and pending hooks
    hooks: Hooks,
    default_timeout: Duration,
    extendet_timeout: Duration,
}

impl Drop for Client {
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

fn on_reconnect(hooks: Hooks) {}

fn match_hook(method: &str, hook: &mut TaskHooks) -> Option<BoxFuture<'static, ()>> {
    match method {
        // "aria2.onDownloadStart" => &mut hook.on_start,
        // "aria2.onDownloadPause" => &mut hook.on_pause,
        // "aria2.onDownloadStop" => &mut hook.on_stop,
        "aria2.onDownloadComplete" => &mut hook.on_complete,
        "aria2.onDownloadError" => &mut hook.on_error,
        "aria2.onBtDownloadComplete" => &mut hook.on_bt_complete,
        _ => return None,
    }
    .take()
}

fn process_nofitications(req: RpcRequest, hooks: &Hooks) -> Result<(), Error> {
    let err = "unexpected notification";
    let gid = req
        .params
        .get(0)
        .ok_or(err)?
        .get("gid")
        .ok_or(err)?
        .as_str()
        .ok_or(err)?;
    let mut lock = hooks.lock().unwrap();
    if let Some(hook) = lock.0.get_mut(gid) {
        if let Some(hook) = match_hook(req.method.as_str(), hook) {
            drop(lock);
            spawn(hook);
        }
    } else {
        match lock.1.entry(gid.to_owned()) {
            Entry::Occupied(mut e) => {
                e.get_mut().insert(req.method);
            }
            Entry::Vacant(e) => {
                let mut set = HashSet::with_capacity(1);
                set.insert(req.method);
                e.insert(set);
            }
        }
    }
    Ok(())
}

async fn read_worker(
    mut read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    subscribes: Arc<Mutex<HashMap<u64, oneshot::Sender<RpcResponse>>>>,
    hooks: Hooks,
) {
    while let Ok(Some(msg)) = read.try_next().await {
        let s = try_continue!(msg.to_text());
        let v = try_continue!(serde_json::from_str::<Value>(s), s);
        if let Value::Object(obj) = &v {
            if obj.contains_key("method") {
                // The message is a notification.
                // https://aria2.github.io/manual/en/html/aria2c.html#notifications
                let req: RpcRequest = try_continue!(serde_json::from_value(v), s);
                try_continue!(process_nofitications(req, &hooks), s);
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
}

impl Client {
    pub async fn connect(url: &str, token: Option<&str>) -> Result<Self, Error> {
        let (tx_write, mut rx_write) = mpsc::channel::<Message>(4);
        let subscribes: Arc<Mutex<HashMap<u64, oneshot::Sender<RpcResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let hooks: Hooks = Arc::new(Mutex::new((HashMap::new(), HashMap::new())));
        let shutdown = Arc::new(Notify::new());
        let url = url.to_string();

        {
            let subscribes = subscribes.clone();
            let hooks = hooks.clone();
            let shutdown = shutdown.clone();
            spawn(async move {
                loop {
                    match tokio_tungstenite::connect_async(&url).await {
                        Ok((ws, _)) => {
                            let (mut write, read) = ws.split();

                            let read_fut =
                                spawn(read_worker(read, subscribes.clone(), hooks.clone()));
                            let write_fut = async move {
                                while let Some(msg) = rx_write.recv().await {
                                    try_continue!(write.send(msg).await);
                                    try_continue!(write.flush().await);
                                }
                                rx_write
                            };

                            select! {
                                rx = write_fut => {
                                    rx_write = rx;
                                    read_fut.abort();
                                    subscribes.lock().unwrap().clear();
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

        // TODO: Auto reconnect, can be shut down
        Ok(Self {
            tx_write,
            id: AtomicU64::new(0),
            token: token.map(|t| "token:".to_string() + t),
            subscribes,
            shutdown,
            hooks,
            default_timeout: Duration::from_secs(15),
            extendet_timeout: Duration::from_secs(120),
        })
    }

    pub fn set_token(&mut self, token: Option<&str>) {
        self.token = token.map(|t| "token:".to_string() + t);
    }

    fn id(&self) -> u64 {
        self.id.fetch_add(1, SeqCst)
    }

    pub async fn call(&self, id: u64, method: &str, mut params: Vec<Value>) -> Result<(), Error> {
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

    fn subscribe_id<T>(&self, id: u64, timeout: Option<Duration>) -> BoxFuture<Result<T, Error>>
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

    fn set_hooks(&self, gid: &str, hooks: Option<TaskHooks>) {
        if let Some(mut hook) = hooks {
            let gid = gid.to_owned();
            let mut lock = self.hooks.lock().unwrap();
            if let Some(set) = lock.1.get_mut(&gid) {
                let set = std::mem::take(set);
                for method in set {
                    if let Some(h) = match_hook(&method, &mut hook) {
                        spawn(h);
                    }
                }
            }
            lock.0.insert(gid, hook);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Client, Error};
    #[tokio::test]
    async fn it_works() -> Result<(), Error> {
        let client = Client::connect("ws://127.0.0.1:6800/jsonrpc", None).await?;
        let r = client.get_version().await?;
        println!("{:?}", r);
        Ok(())
    }
}
