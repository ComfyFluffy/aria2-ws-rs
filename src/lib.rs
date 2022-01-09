//!
//! An aria2 websocket jsonrpc in Rust.
//!
//! [aria2 RPC docs](https://aria2.github.io/manual/en/html/aria2c.html#methods)
//!
//! ## Features
//!
//! - Almost all methods and structed responses
//! - Auto reconnect
//! - Ensures `on_complete` and `on_error` hook to be executed even after reconnected.
//! - Supports notifications
//!
//! ## Example
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use aria2_ws::{Client, TaskHooks, TaskOptions};
//! use futures::FutureExt;
//! use serde_json::json;
//! use tokio::{spawn, sync::Semaphore};
//!
//! #[tokio::main]
//! async fn main() {
//!     let client = Client::connect("ws://127.0.0.1:6800/jsonrpc", None)
//!         .await
//!         .unwrap();
//!     let options = TaskOptions {
//!         split: Some(2),
//!         header: Some(vec!["Referer: https://www.pixiv.net/".to_string()]),
//!         all_proxy: Some("http://127.0.0.1:10809".to_string()),
//!         // Add extra options which are not included in TaskOptions.
//!         extra_options: json!({"max-download-limit": "200K"})
//!             .as_object()
//!             .unwrap()
//!             .clone(),
//!         ..Default::default()
//!     };
//!
//!     // use `tokio::sync::Semaphore` to wait for all tasks to finish.
//!     let semaphore = Arc::new(Semaphore::new(0));
//!     client
//!         .add_uri(
//!             vec![
//!                 "https://i.pximg.net/img-original/img/2020/05/15/06/56/03/81572512_p0.png"
//!                     .to_string(),
//!             ],
//!             Some(options.clone()),
//!             None,
//!             Some(TaskHooks {
//!                 on_complete: Some({
//!                     let s = semaphore.clone();
//!                     async move {
//!                         s.add_permits(1);
//!                         println!("Task 1 completed!");
//!                     }
//!                     .boxed()
//!                 }),
//!                 on_error: Some({
//!                     let s = semaphore.clone();
//!                     async move {
//!                         s.add_permits(1);
//!                         println!("Task 1 error!");
//!                     }
//!                     .boxed()
//!                 }),
//!             }),
//!         )
//!         .await
//!         .unwrap();
//!
//!     // Will 404
//!     client
//!         .add_uri(
//!             vec![
//!                 "https://i.pximg.net/img-original/img/2022/01/05/23/32/16/95326322_p0.pngxxxx"
//!                     .to_string(),
//!             ],
//!             Some(options.clone()),
//!             None,
//!             Some(TaskHooks {
//!                 on_complete: Some({
//!                     let s = semaphore.clone();
//!                     async move {
//!                         s.add_permits(1);
//!                         println!("Task 2 completed!");
//!                     }
//!                     .boxed()
//!                 }),
//!                 on_error: Some({
//!                     let s = semaphore.clone();
//!                     async move {
//!                         s.add_permits(1);
//!                         println!("Task 2 error!");
//!                     }
//!                     .boxed()
//!                 }),
//!             }),
//!         )
//!         .await
//!         .unwrap();
//!
//!     let mut not = client.subscribe_notifications();
//!
//!     spawn(async move {
//!         loop {
//!             if let Ok(msg) = not.recv().await {
//!                 println!("Received notification {:?}", &msg);
//!             } else {
//!                 return;
//!             }
//!         }
//!     });
//!
//!     // Wait for 2 tasks to finish.
//!     let _ = semaphore.acquire_many(2).await.unwrap();
//!
//!     client.shutdown().await.unwrap();
//! }
//!
//! ```

mod client;
mod method;
pub mod response;
mod utils;

use std::collections::{HashMap, HashSet};

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use std::time::Duration;

use futures::future::BoxFuture;
use response::Event;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use tokio::sync::{broadcast, Notify};

use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use utils::serde_option_from_str;

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
///     let version = client.get_version().await.unwrap();
///     println!("{:?}", version);
/// }
/// ```
#[derive(Clone)]
pub struct Client(Arc<InnerClient>);

impl Drop for InnerClient {
    fn drop(&mut self) {
        self.shutdown.notify_waiters();
    }
}

type Error = Box<dyn std::error::Error + Send + Sync>;
