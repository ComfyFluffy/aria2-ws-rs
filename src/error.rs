use snafu::prelude::*;
use tokio::{
    sync::{mpsc, oneshot},
    time::error::Elapsed,
};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::RpcResponse;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    Aria2 {
        source: crate::Aria2Error,
    },
    #[snafu(display("aria2: unexpected message received: {msg:?}"))]
    UnexpectedMessage {
        msg: Message,
    },

    ResultNotFound {
        response: RpcResponse,
    },
    Websocket {
        source: WsError,
    },
    Json {
        source: serde_json::Error,
    },
    Timeout {
        source: Elapsed,
    },
    OneshotRecv {
        source: oneshot::error::RecvError,
    },
    MpscSendMessage {
        source: mpsc::error::SendError<Message>,
    },
    ReconnectHook {
        message: String,
    },
}
