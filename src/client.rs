use crate::{
    error, Client, InnerClient, Notification, Result, RpcRequest, RpcResponse, Subscription,
};
use futures::prelude::*;
use log::{debug, info};
use serde::de::DeserializeOwned;
use serde_json::Value;
use snafu::prelude::*;
use std::{
    collections::HashMap,
    ops::Deref,
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    select, spawn,
    sync::{broadcast, mpsc, oneshot, Notify},
    time::sleep,
};
use tokio_tungstenite::tungstenite::Message;

type WebSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Print error if the result is an Err.
fn print_error<E>(res: std::result::Result<(), E>)
where
    E: std::fmt::Display,
{
    if let Err(err) = res {
        info!("{}", err);
    }
}

/// Checks all stopped tasks to see if some hooks need to be called.
// async fn callback_worker(
//     inner_client: Weak<InnerClient>,
//     rx_notification: mpsc::Receiver<Notification>,
// ) {
//     // Response from `custom_tell_stopped` call
//     #[derive(Debug, Clone, Deserialize)]
//     #[serde(rename_all = "camelCase")]
//     struct TaskStatus {
//         status: String,
//         total_length: String,
//         completed_length: String,
//         gid: String,
//     }

//     if let Some(client) = inner_client.upgrade() {
//         let mut res: HashMap<String, TaskStatus> = HashMap::new();
//         // Convert map to TaskStatus.
//         for map in client
//             .custom_tell_stopped(
//                 0,
//                 1000,
//                 Some(
//                     ["status", "totalLength", "completedLength", "gid"]
//                         .into_iter()
//                         .map(|x| x.to_string())
//                         .collect(),
//                     // Convert to Vec<String>
//                 ),
//             )
//             .await?
//         {
//             let r: TaskStatus =
//                 serde_json::from_value(Value::Object(map)).context(error::JsonSnafu)?;

//             res.insert(r.gid.clone(), r);
//         }

//         let mut lock = client.hooks.lock().unwrap();
//         for (gid, hooks) in &mut lock.0 {
//             if let Some(status) = res.get(gid) {
//                 // Check if the task is finished by checking the length.
//                 if status.total_length == status.completed_length {
//                     if let Some(h) = hooks.on_complete.take() {
//                         spawn(h);
//                     }
//                 } else if status.status == "error" {
//                     if let Some(h) = hooks.on_error.take() {
//                         spawn(h);
//                     }
//                 }
//             }
//         }
//     }
// }

async fn process_ws(
    ws: WebSocket,
    rx_write: &mut mpsc::Receiver<Message>,
    tx_notification: broadcast::Sender<Notification>,
    rx_subscription: &mut mpsc::Receiver<Subscription>,
) {
    let (mut sink, mut stream) = ws.split();
    let subscriptions = Mutex::new(HashMap::<i32, oneshot::Sender<RpcResponse>>::new());

    let mut stream_worker = async {
        while let Ok(Some(Message::Text(s))) = stream.try_next().await {
            debug!("websocket received string: {:?}", s);
            print_error((|| -> Result<()> {
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
                    let tx = subscriptions.lock().unwrap().remove(id);
                    if let Some(tx) = tx {
                        let _ = tx.send(res);
                    }
                }
                Ok(())
            })());
        }
    }
    .boxed();

    let mut sink_worker = async {
        while let Some(msg) = rx_write.recv().await {
            debug!("writing message to websocket: {:?}", msg);
            let r = sink.send(msg).await;
            print_error(r);
        }
    }
    .boxed();

    loop {
        select! {
            _ = &mut sink_worker => break,
            _ = &mut stream_worker => break,
            subscription = rx_subscription.recv() => {
                if let Some(subscription) = subscription {
                    subscriptions.lock().unwrap().insert(subscription.id, subscription.tx);
                }
            }
        }
    }

    let _ = tx_notification.send(Notification::WebsocketClosed);
}

impl InnerClient {
    pub async fn connect(url: &str, token: Option<&str>) -> Result<Self> {
        let (tx_ws_sink, mut rx_ws_sink) = mpsc::channel(4);
        let (tx_subscription, mut rx_subscription) = mpsc::channel(4);
        let shutdown = Arc::new(Notify::new());
        // Broadcast notifications to all subscribers.
        // The receiver is dropped cause there is no subscriber for now.
        let (tx_notification, _) = broadcast::channel(16);

        let inner = InnerClient {
            tx_ws_sink,
            id: AtomicI32::new(0),
            token: token.map(|t| "token:".to_string() + t),
            tx_subscription,
            tx_notification: tx_notification.clone(),
            shutdown: shutdown.clone(),
        };

        async fn connect_ws(url: &str) -> Result<WebSocket> {
            debug!("connecting to {}", url);
            let (ws, res) = tokio_tungstenite::connect_async(url)
                .await
                .context(error::WebsocketIoSnafu)?;
            debug!("connected to {}, {:?}", url, res);
            Ok(ws)
        }

        let ws = connect_ws(url).await?;
        let url = url.to_string();
        // The following spawned task following will only hold a weak reference to the inner client.
        spawn(async move {
            let mut ws = Some(ws);
            loop {
                if let Some(ws) = ws.take() {
                    let fut = process_ws(
                        ws,
                        &mut rx_ws_sink,
                        tx_notification.clone(),
                        &mut rx_subscription,
                    );
                    select! {
                        _ = fut => {},
                        _ = shutdown.notified() => {
                            return;
                        },
                    }
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

        Ok(inner)
    }

    fn id(&self) -> i32 {
        self.id.fetch_add(1, Ordering::Relaxed)
    }

    async fn wait_for_id<T>(&self, id: i32, rx: oneshot::Receiver<RpcResponse>) -> Result<T>
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

    pub async fn call(&self, id: i32, method: &str, mut params: Vec<Value>) -> Result<()> {
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

    pub async fn call_and_wait<T>(&self, method: &str, params: Vec<Value>) -> Result<T>
    where
        T: DeserializeOwned + Send,
    {
        let id = self.id();
        let (tx, rx) = oneshot::channel();
        self.tx_subscription
            .send(Subscription { id, tx })
            .await
            .expect("tx_subscription: receiver has been closed");

        self.call(id, method, params).await?;
        self.wait_for_id::<T>(id, rx).await
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Notification> {
        self.tx_notification.subscribe()
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
    pub async fn connect(url: &str, token: Option<&str>) -> Result<Self> {
        let inner = Arc::new(InnerClient::connect(url, token).await?);
        Ok(Self(inner))
    }
}

impl Deref for Client {
    type Target = InnerClient;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
