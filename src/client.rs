use crate::{error, Client, Error, InnerClient, RpcRequest, RpcResponse, TaskCallbacks};
use futures::prelude::*;
use log::{debug, info};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;
use snafu::prelude::*;
use std::{
    collections::HashMap,
    sync::{
        atomic::{
            AtomicI32,
            Ordering::{self},
        },
        Arc, Weak,
    },
    time::Duration,
};
use tokio::{
    select, spawn,
    sync::{broadcast, mpsc, oneshot, Notify},
    time::sleep,
};
use tokio_tungstenite::tungstenite::Message;

use crate::response::Notification;

type WebSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Print error if the result is an Err.
fn print_error<E>(res: Result<(), E>)
where
    E: std::fmt::Display,
{
    if let Err(err) = res {
        info!("{}", err);
    }
}

/// Checks all stopped tasks to see if some hooks need to be called.
async fn callback_worker(
    inner_client: Weak<InnerClient>,
    rx_notification: mpsc::Receiver<Notification>,
) {
    // Response from `custom_tell_stopped` call
    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TaskStatus {
        status: String,
        total_length: String,
        completed_length: String,
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
                if status.total_length == status.completed_length {
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
}

impl InnerClient {
    fn id(&self) -> i32 {
        self.id.fetch_add(1, Ordering::Relaxed)
    }

    async fn wait_for_id<T>(&self, id: i32, rx: oneshot::Receiver<RpcResponse>) -> Result<T, Error>
    where
        T: DeserializeOwned + Send,
    {
        let res = rx.await.map_err(|err| {
            error::WebsocketClosedSnafu {
                message: format!("receiving response for id {}: {}", id, err),
            }
            .build()
        })?;

        if let Some(err) = res.error {
            return Err(err).context(error::Aria2Snafu);
        }

        if let Some(v) = res.result {
            Ok(serde_json::from_value::<T>(v).context(error::JsonSnafu)?)
        } else {
            error::ParseSnafu {
                value: format!("{:?}", res),
                to: "RpcResponse with result",
            }
            .fail()
        }
    }

    async fn call(&self, id: i32, method: &str, mut params: Vec<Value>) -> Result<(), Error> {
        if let Some(ref token) = self.token {
            params.insert(0, Value::String(token.clone()))
        }
        let req = RpcRequest {
            id: Some(id),
            jsonrpc: "2.0".to_string(),
            method: "aria2.".to_string() + method,
            params,
        };
        self.tx_ws_sink
            .send(Message::Text(
                serde_json::to_string(&req).context(error::JsonSnafu)?,
            ))
            .await
            .expect("tx_ws_sink: receiver has been dropped");
        Ok(())
    }

    pub async fn call_and_wait<T>(&self, method: &str, params: Vec<Value>) -> Result<T, Error>
    where
        T: DeserializeOwned + Send,
    {
        let id = self.id();
        let (tx, rx) = oneshot::channel();
        self.tx_subscribe
            .send((id, tx))
            .await
            .expect("tx_subscribe: receiver has been closed");

        self.call(id, method, params).await?;
        self.wait_for_id::<T>(id, rx).await
    }
}

async fn process_ws(
    ws: WebSocket,
    rx_write: &mut mpsc::Receiver<Message>,
    tx_notification: broadcast::Sender<Notification>,
    subscriptions: &mut HashMap<i32, oneshot::Sender<RpcResponse>>,
) {
    let (mut sink, mut stream) = ws.split();

    let stream_worker = async {
        while let Ok(Some(Message::Text(s))) = stream.try_next().await {
            debug!("websocket received string: {:?}", s);
            print_error((|| -> Result<(), Error> {
                let v: Value = serde_json::from_str(&s).context(error::JsonSnafu)?;
                if let Value::Object(obj) = &v {
                    if obj.contains_key("method") {
                        // The message should be a notification.
                        // https://aria2.github.io/manual/en/html/aria2c.html#notifications
                        let req: RpcRequest =
                            serde_json::from_value(v).context(error::JsonSnafu)?;
                        let notification = (&req).try_into()?;
                        let _ = tx_notification.send(notification);
                        return Ok(());
                    }
                }

                // The message should be a response.
                let res: RpcResponse = serde_json::from_value(v).context(error::JsonSnafu)?;
                if let Some(ref id) = res.id {
                    let tx = subscriptions.remove(id);
                    if let Some(tx) = tx {
                        let _ = tx.send(res);
                    }
                }
                Ok(())
            })());
        }
    };

    let sink_worker = async {
        while let Some(msg) = rx_write.recv().await {
            debug!("writing message to websocket: {:?}", msg);
            let r = sink.send(msg).await;
            print_error(r);
        }
    };

    // Run two tasks concurrently.
    select! {
        _ = sink_worker => {},
        _ = stream_worker => {},
    }

    let _ = tx_notification.send(Notification::WebsocketClosed);
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
        let (tx_ws_sink, mut rx_ws_sink) = mpsc::channel(4);
        let (tx_subscribe, rx_subscribe) = mpsc::channel(4);
        let shutdown = Arc::new(Notify::new());
        // Broadcast notifications to all subscribers.
        // The receiver is dropped cause there is no subscriber for now.
        let (tx_notification, _) = broadcast::channel(16);

        let inner = Arc::new(InnerClient {
            tx_ws_sink,
            id: AtomicI32::new(0),
            token: token.map(|t| "token:".to_string() + t),
            tx_subscribe,
            tx_notification: tx_notification.clone(),
            shutdown: shutdown.clone(),
        });

        async fn connect_ws(url: &str) -> Result<WebSocket, Error> {
            debug!("connecting to {}", url);
            let (ws, res) = tokio_tungstenite::connect_async(url)
                .await
                .context(error::WebsocketIoSnafu)?;
            debug!("connected to {}, {:?}", url, res);
            Ok(ws)
        }

        let ws = connect_ws(url).await?;
        let inner_weak = Arc::downgrade(&inner);
        let url = url.to_string();
        // The following spawned task following will only hold a weak reference to the inner client.
        spawn(async move {
            let mut ws = Some(ws);
            let mut subscriptions = HashMap::new();
            loop {
                if let Some(ws) = ws.take() {
                    let fut = process_ws(
                        ws,
                        &mut rx_ws_sink,
                        tx_notification.clone(),
                        &mut subscriptions,
                    );
                    select! {
                        _ = fut => {},
                        _ = shutdown.notified() => {
                            return;
                        },
                    }

                    // Disconnected. Make current subscriptions invalid.
                    subscriptions.clear();
                } else {
                    let r = select! {
                        r = connect_ws(&url) => r,
                        _ = shutdown.notified() => return,
                    };
                    match r {
                        Ok(ws_) => {
                            ws.replace(ws_);
                        }
                        Err(err) => {
                            info!("{}", err);
                            sleep(Duration::from_secs(3)).await;
                        }
                    }
                }
            }
        });

        Ok(Self(inner))
    }

    // Call aria2 without waiting for the response.
    pub async fn call(&self, id: i32, method: &str, params: Vec<Value>) -> Result<(), Error> {
        self.0.call(id, method, params).await
    }

    /// Call aria2 with the given method and params,
    /// and return the result in timeout.
    /// If the timeout is None, there is no timeout.
    pub async fn call_and_wait<T: DeserializeOwned + Send>(
        &self,
        method: &str,
        params: Vec<Value>,
    ) -> Result<T, Error> {
        self.0.call_and_wait(method, params).await
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Notification> {
        self.0.tx_notification.subscribe()
    }

    pub async fn add_callbacks(&self, gid: &str, callbacks: TaskCallbacks) -> Result<(), Error> {
        Ok(())
    }
}
