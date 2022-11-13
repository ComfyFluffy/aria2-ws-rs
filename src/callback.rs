use futures::future::BoxFuture;

use crate::{Client, Result};

type Callback = Option<BoxFuture<'static, ()>>;

/// Hooks that will be executed on notifications.
///
/// If the connection lost, all hooks will be checked whether they need to be executed once reconnected.
#[derive(Default)]
pub struct TaskCallbacks {
    pub on_download_complete: Callback,
    pub on_error: Callback,
}

impl TaskCallbacks {
    pub fn is_some(&self) -> bool {
        self.on_download_complete.is_some() || self.on_error.is_some()
    }
}

impl Client {
    pub(crate) async fn add_callbacks(&self, gid: &str, callbacks: TaskCallbacks) -> Result<()> {
        unimplemented!()
    }
}
