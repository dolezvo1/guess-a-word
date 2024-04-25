
use std::sync::mpsc::Sender;
use tokio::task::{spawn_blocking, JoinHandle};

use crate::util::InternalMessage;

/// Spawn a task to block on stdin read
pub fn spawn_stdin_listener<T>(transmitter: Sender<InternalMessage<T>>) -> JoinHandle<()>
    where T: Send + 'static
{
    spawn_blocking(move || { loop {
        let mut buffer = String::new();
        std::io::stdin().read_line(&mut buffer).unwrap();
        buffer = buffer.strip_suffix(if cfg!(windows) {"\r\n"} else {"\n"})
                       .unwrap().to_string();
        if transmitter.send(InternalMessage::StdIn(buffer)).is_err() {
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
