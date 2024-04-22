
use std::sync::mpsc::Sender;
use serde::Deserialize;
use tokio::task::{spawn_blocking, JoinHandle};
use bincode::ErrorKind;

use crate::util::{impl_spawn_network_listener, ProtocolReader};

/// Internal message type - type of messages flowing between threads on server machine
pub enum ServerInternalMessage<T> {
    Network(Result<T,()>),
    Opponent(T),
    OpponentAssigned(/*tx: */Sender<ServerInternalMessage<T>>,
                     /*id:*/String, /*word: */String),
    WordFound,
    OpponentDisconnected,
    Error,
}

impl<T> ServerInternalMessage<T> {
    impl_spawn_network_listener!(pub);
}
