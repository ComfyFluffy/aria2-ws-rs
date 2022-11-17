use std::{collections::HashMap, fmt, sync::Weak, time::Duration};

use futures::future::BoxFuture;
use log::{debug, info};
use serde::Deserialize;
use snafu::ResultExt;
use tokio::{
    select, spawn,
    sync::{broadcast, mpsc},
    time::timeout,
};

use crate::{error, utils::print_error, Event, InnerClient, Notification, Result};

type Callback = Option<BoxFuture<'static, ()>>;

/// Callbacks that will be executed on notifications.
///
/// If the connection lost, all callbacks will be checked whether they need to be executed once reconnected.
#[derive(Default)]
pub struct Callbacks {
    /// Will trigger on `Event::Complete` or `Event::BtComplete`.
    pub on_download_complete: Callback,
    /// Will trigger on `Event::Error`.
    pub on_error: Callback,
}

impl fmt::Debug for Callbacks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Callbacks")
            .field("on_download_complete", &self.on_download_complete.is_some())
            .field("on_error", &self.on_error.is_some())
            .finish()
    }
}

/// Check whether the callback is ready to be executed after reconnected.
async fn on_reconnect(
    inner: &InnerClient,
    callbacks_map: &mut HashMap<String, Callbacks>,
) -> Result<()> {
    // Response from `custom_tell_stopped` call
    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TaskStatus {
        status: String,
        total_length: String,
        completed_length: String,
        gid: String,
    }

    if callbacks_map.is_empty() {
        return Ok(());
    }
    let mut tasks = HashMap::new();
    let req = inner.custom_tell_stopped(
        0,
        1000,
        Some(
            ["status", "totalLength", "completedLength", "gid"]
                .into_iter()
                .map(|x| x.to_string())
                .collect(),
        ),
    );
    // Cancel if takes too long
    for map in timeout(Duration::from_secs(10), req)
        .await
        .context(error::ReconnectTaskTimeoutSnafu)??
    {
        let task: TaskStatus =
            serde_json::from_value(serde_json::Value::Object(map)).context(error::JsonSnafu)?;
        tasks.insert(task.gid.clone(), task);
    }

    for (gid, callbacks) in callbacks_map {
        if let Some(status) = tasks.get(gid) {
            debug!("checking callbacks for gid {} after reconnected", gid);
            // Check if the task is finished by checking the length.
            if status.total_length == status.completed_length {
                if let Some(h) = callbacks.on_download_complete.take() {
                    spawn(h);
                }
            } else if status.status == "error" {
                if let Some(h) = callbacks.on_error.take() {
                    spawn(h);
                }
            }
        }
    }

    Ok(())
}

async fn on_aria2_notification(
    gid: String,
    event: Event,
    callbacks_map: &mut HashMap<String, Callbacks>,
) {
    if let Some(callbacks) = callbacks_map.get_mut(&gid) {
        match event {
            Event::Complete | Event::BtComplete => {
                if let Some(callback) = callbacks.on_download_complete.take() {
                    // Spawn a new task to avoid blocking the notification receiver.
                    spawn(callback);
                }
            }
            Event::Error => {
                if let Some(callback) = callbacks.on_error.take() {
                    spawn(callback);
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug)]
pub(crate) struct TaskCallbacks {
    pub gid: String,
    pub callbacks: Callbacks,
}

pub(crate) async fn callback_worker(
    weak: Weak<InnerClient>,
    mut rx_notification: broadcast::Receiver<Notification>,
    mut rx_callback: mpsc::Receiver<TaskCallbacks>,
) {
    use broadcast::error::RecvError;

    let mut is_first_notification = true;
    let mut callbacks_map = HashMap::new();

    loop {
        select! {
            r = rx_notification.recv() => {
                match r {
                    Ok(notification) => {
                        match notification {
                            Notification::WebSocketConnected => {
                                if is_first_notification {
                                    is_first_notification = false;
                                    continue;
                                    // Skip the first connected notification
                                }
                                // We might miss some notifications when the connection is lost.
                                // So we need to check whether the callbacks need to be executed after reconnected.
                                if let Some(inner) = weak.upgrade() {
                                    print_error(on_reconnect(inner.as_ref(), &mut callbacks_map).await);
                                }
                            },
                            Notification::Aria2 { gid, event } => {
                                on_aria2_notification(gid, event, &mut callbacks_map).await;
                            },
                            _ => {}
                        }
                    }
                    Err(RecvError::Closed) => {
                        return;
                    }
                    Err(RecvError::Lagged(_)) => {
                        info!("unexpected lag in notifications");
                    }
                }
            },
            r = rx_callback.recv() => {
                match r {
                    Some(TaskCallbacks { gid, callbacks }) => {
                        callbacks_map.insert(gid, callbacks);
                    }
                    None => {
                        return;
                    }
                }
            },
        }
    }
}
