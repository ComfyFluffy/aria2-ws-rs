use crate::{
    error, response::Event, Client, Error, Hooks, InnerClient, RpcRequest, RpcResponse, TaskHooks,
};
use futures::{
    future::BoxFuture,
    prelude::*,
    stream::{SplitSink, SplitStream},
    StreamExt, TryStreamExt,
};
use log::{debug, info};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;
use snafu::prelude::*;
use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{
        atomic::{AtomicU64, Ordering::SeqCst},
        Arc, Mutex, Weak,
    },
    time::Duration,
};
use tokio::{
    net::TcpStream,
    select, spawn,
    sync::{broadcast, mpsc, oneshot, Notify},
    time::sleep,
};
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

use crate::response::Notification;

macro_rules! try_continue {
    ($res:expr) => {
        match $res {
            Ok(v) => v,
            Err(err) => {
                info!("{}", err);
                continue;
            }
        }
    };
}

/// Print error if the result is an Err.
fn print_error<E>(res: Result<(), E>)
where
    E: std::error::Error,
{
    if let Err(err) = res {
        info!("{}", err);
    }
}

/// Checks all stopped tasks to see if some hooks need to be called.
async fn on_reconnect(inner_client: Weak<InnerClient>) -> Result<(), Error> {
    // Response from `custom_tell_stopped` call
    #[allow(non_snake_case)]
    #[derive(Debug, Clone, Deserialize)]
    struct TaskStatus {
        status: String,
        totalLength: String,
        completedLength: String,
        gid: String,
    }

    if let Some(client) = inner_client.upgrade() {
        let mut res: HashMap<String, TaskStatus> = HashMap::new();
        // Convert map to TaskStatus.
        for map in client
            .custom_tell_stopped(
                0,
                1000,
                Some(
                    ["status", "totalLength", "completedLength", "gid"]
                        .into_iter()
                        .map(|x| x.to_string())
                        .collect(),
                    // Convert to Vec<String>
                ),
            )
            .await?
        {
            let r: TaskStatus =
                serde_json::from_value(Value::Object(map)).context(error::JsonSnafu)?;

            res.insert(r.gid.clone(), r);
        }

        let mut lock = client.hooks.lock().unwrap();
        for (gid, hooks) in &mut lock.0 {
            if let Some(status) = res.get(gid) {
                // Check if the task is finished by checking the length.
                if status.totalLength == status.completedLength {
                    if let Some(h) = hooks.on_complete.take() {
                        spawn(h);
                    }
                } else if status.status == "error" {
                    if let Some(h) = hooks.on_error.take() {
                        spawn(h);
                    }
                }
            }
        }
    }
    Ok(())
}

fn take_hook(event: Event, hook: &mut TaskHooks) -> Option<BoxFuture<'static, ()>> {
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
        if let Some(hook) = take_hook(notification.event, hook) {
            spawn(hook);
        }
    } else {
        match lock.1.entry(notification.gid.clone()) {
            Entry::Occupied(mut e) => {
                e.get_mut().insert(notification.event);
            }
            Entry::Vacant(e) => {
                e.insert([notification.event].into_iter().collect());
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
    ) -> impl Future<Output = Result<T, Error>>
    where
        T: DeserializeOwned + Send,
    {
        let (tx, rx) = oneshot::channel();
        self.subscriptions.lock().unwrap().insert(id, tx);
        let timeout = timeout.unwrap_or(self.default_timeout);
        let subscriptions = self.subscriptions.clone();

        async move {
            let res = if timeout.is_zero() {
                rx.await.context(error::OneshotRecvSnafu)?
            } else {
                match tokio::time::timeout(timeout, rx).await {
                    Ok(r) => r.context(error::OneshotRecvSnafu)?,
                    Err(e) => {
                        subscriptions.lock().unwrap().remove(&id);
                        // Timed out. Clean up.
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
        // subscribe before calling
        self.call(id, method, params).await?;
        fut.await
    }
}

async fn read_worker(
    mut read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    subscriptions: Arc<Mutex<HashMap<u64, oneshot::Sender<RpcResponse>>>>,
    hooks: Hooks,
    tx_not: broadcast::Sender<Notification>,
) -> Result<(), Error> {
    while let Some(Message::Text(s)) = read.try_next().await.context(error::WebsocketSnafu)? {
        print_error((|| -> Result<(), Error> {
            let v: Value = serde_json::from_str(&s).context(error::JsonSnafu)?;
            if let Value::Object(obj) = &v {
                if obj.contains_key("method") {
                    // The message is a notification.
                    // https://aria2.github.io/manual/en/html/aria2c.html#notifications
                    let errf = || error::UnexpectedMessageSnafu { message: s.clone() };
                    let req: RpcRequest = serde_json::from_value(v).context(error::JsonSnafu)?;
                    let gid = get_gid_from_notifictaion(&req).with_context(errf)?;
                    let not = Notification::new(gid.to_string(), &req.method).with_context(errf)?;

                    process_nofitications(&not, &hooks)?;

                    let _ = tx_not.send(not);
                    return Ok(());
                }
            }

            // The message can be a response.
            let res: RpcResponse = serde_json::from_value(v).context(error::JsonSnafu)?;
            if let Some(ref id) = res.id {
                let tx = subscriptions.lock().unwrap().remove(id);
                if let Some(tx) = tx {
                    let _ = tx.send(res);
                }
            }
            Ok(())
        })());
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
                    // The sender is closed, which means the client is dropped.
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
    /// Create a new `Client` that connects to the given url.
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
        let url = url.to_string();
        let (tx_write, mut rx_write) = mpsc::channel::<Message>(4);
        // channel for sending messages to the aria2 server.
        let subscriptions: Arc<Mutex<HashMap<u64, oneshot::Sender<RpcResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        // Used for storing subscriptions.
        // On receiving a message, subscription with the same id to the message id will be removed and processed.
        let hooks: Hooks = Arc::new(Mutex::new((HashMap::new(), HashMap::new())));
        // The first hashmap stores the hooks, and the second hashmap stores the penging events for extra hooks run check.
        let shutdown = Arc::new(Notify::new());
        // sync all spawned tasks to shutdown
        let (tx_not, _) = broadcast::channel(16);
        // Broadcast notifications to all subscribers.
        // The receiver is dropped cause no one subscribes for now.
        // The notifications can be received again by calling tx_not.subscribe().

        let inner = Arc::new(InnerClient {
            tx_write,
            id: AtomicU64::new(0),
            token: token.map(|t| "token:".to_string() + t),
            subscriptions: subscriptions.clone(),
            shutdown: shutdown.clone(),
            hooks: hooks.clone(),
            default_timeout: Duration::from_secs(10),
            extended_timeout: Duration::from_secs(120),
            tx_not: tx_not.clone(),
        });

        let inner_weak = Arc::downgrade(&inner);
        // The following spawned task following will only hold a weak reference to the inner client.
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
                        let read_fut =
                            read_worker(read, subscriptions.clone(), hooks.clone(), tx_not.clone());
                        // `read_fut` will be dropped if current task is stopped.

                        let exit = Arc::new(Notify::new());
                        let write_fut = spawn(write_worker(write, rx_write, exit.clone()));

                        if reconnect {
                            spawn(on_reconnect(inner_weak.clone()));
                        } else {
                            reconnect = true;
                            // run `on_reconnect` task next time.
                        }

                        select! {
                            result = read_fut => {
                                info!("aria2 disconnected: {:?}", result);
                                exit.notify_waiters();
                                // notify write_worker to exit.
                                rx_write = write_fut.await.unwrap();
                                // re-initialize rx_write for the next connection.
                            },
                            _ = shutdown.notified() => {
                                debug!("aria2 client is exiting");
                                exit.notify_waiters();
                                return;
                            }
                        }
                    }
                    Err(err) => {
                        info!("aria2: connect failed: {}", err);
                        sleep(Duration::from_secs(3)).await;
                    }
                }
            }
        });

        Ok(Self(inner))
    }

    // Call aria2 without waiting for the response.
    pub async fn call(&self, id: u64, method: &str, params: Vec<Value>) -> Result<(), Error> {
        self.0.call(id, method, params).await
    }

    /// Call aria2 with the given method and params,
    /// and return the result in timeout.
    ///
    /// If the timeout is `None`, the default timeout will be used.
    ///
    /// If the timeout is zero, there will be no timeout.
    pub async fn call_and_subscribe<T: DeserializeOwned + Send>(
        &self,
        method: &str,
        params: Vec<Value>,
        timeout: Option<Duration>,
    ) -> Result<T, Error> {
        self.0.call_and_subscribe(method, params, timeout).await
    }

    pub async fn set_hooks(&self, gid: &str, hooks: Option<TaskHooks>) {
        if let Some(mut hooks) = hooks {
            if !hooks.is_some() {
                return;
            }
            let mut lock = self.0.hooks.lock().unwrap();
            if let Some(set) = lock.1.remove(gid) {
                for event in set {
                    if let Some(h) = take_hook(event, &mut hooks) {
                        spawn(h);
                    }
                }
            }
            if hooks.is_some() {
                lock.0.insert(gid.to_string(), hooks);
            }
        }
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Notification> {
        self.0.tx_not.subscribe()
    }
}
