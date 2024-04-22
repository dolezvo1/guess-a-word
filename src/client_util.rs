
use std::sync::mpsc::Sender;
use serde::Deserialize;
use tokio::task::{spawn_blocking, JoinHandle};
use bincode::ErrorKind;

use crate::util::{impl_spawn_network_listener, ProtocolReader};

/// Internal message type - type of messages flowing between threads on client machine
pub enum ClientInternalMessage<T> {
    Network(Result<T,()>),
    StdIn(String),
    Error,
}

impl<T> ClientInternalMessage<T> {
    impl_spawn_network_listener!(pub);
}

/// Spawn a task to block on stdin read
pub fn spawn_stdin_listener<T>(transmitter: Sender<ClientInternalMessage<T>>) -> JoinHandle<()>
    where T: Send + 'static
{
    spawn_blocking(move || { loop {
        let mut buffer = String::new();
        std::io::stdin().read_line(&mut buffer).unwrap();
        buffer = buffer.strip_suffix(if cfg!(windows) {"\r\n"} else {"\n"})
                       .unwrap().to_string();
        if transmitter.send(ClientInternalMessage::StdIn(buffer)).is_err() {
            break
        }
    }})
}

/// Read user input from stdin following a prompt
macro_rules! prompted_input { ($prompt: expr) => {{
    print!("{}", $prompt);
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    
    line.strip_suffix(if cfg!(windows) {"\r\n"} else {"\n"}).unwrap().to_string()
}}}
pub(crate) use prompted_input;
