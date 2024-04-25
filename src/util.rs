
use std::sync::mpsc::Sender;
use serde::Deserialize;
use tokio::task::{spawn_blocking, JoinHandle};
use bincode::ErrorKind;

use crate::protocol::ProtocolReader;

/// Find named argument, parse it to T
pub fn parse_arg<T: std::str::FromStr>(
    name: &str,
    args: &[String],
    options: &[&str],
) -> Option<T> {
    let mut it = args.iter();
    while let Some(value) = it.next() {
        if value == name {
            return it.next().and_then(|e| e.parse::<T>().ok());
        } else if options.iter().any(|e| *e == value) {
            it.next();
        }
    }
    None
}

/// Internal message type - type of messages flowing between threads on server machine
#[allow(dead_code)]
pub enum InternalMessage<T> {
    Network(Result<T,()>),
    Opponent(T),
    OpponentAssigned(/*tx: */Sender<InternalMessage<T>>,
                     /*id:*/String, /*word: */String),
    WordFound,
    OpponentDisconnected,
    StdIn(String),
    Error,
}

/// Spawn a task to block on network reader read
/// Sends InternalMessage::Network(Ok(a)) when message is read,
///       InternalMessage::Network(Err(())) when message deserialization fails,
///   and InternalMessage::Error on unrecoverable error (such as lost connection).
pub fn spawn_network_listener<R, T>(
    mut reader: Box<R>,
    transmitter: Sender<InternalMessage<T>>,
) -> JoinHandle<()>
    where R: ProtocolReader<T> + Send + ?Sized + 'static,
          T: for<'a> Deserialize<'a> + Send + 'static
{
    spawn_blocking(move || { loop {
        match reader.as_mut().read() {
            Ok(a) => {
                transmitter.send(InternalMessage::Network(Ok(a))).unwrap();
            },
            Err(e) => match *e {
                ErrorKind::Io(_) => {
                    transmitter.send(InternalMessage::Error).unwrap();
                    break;
                },
                _ => {
                    transmitter.send(InternalMessage::Network(Err(()))).unwrap();
                },
            }
        }
    }})
}


#[cfg(test)]
mod tests {
    use crate::parse_arg;
    
    fn arg_set_1() -> Vec<String> {
        vec!["--received-option".to_string(), "value1".to_string(),
             "--different-received-option".to_string(), "value2".to_string(),
             "--last-received-option".to_string(), "value3".to_string()]
    }
    fn arg_set_2() -> Vec<String> {
        vec!["--received-option".to_string(), "--different-received-option".to_string(),
             "--different-received-option".to_string(), "value2".to_string(),
             "--last-received-option".to_string(), "value3".to_string()]
    }
    fn empty() -> Vec<&'static str> { vec![] }
    fn accepted_options() -> Vec<&'static str> {
        vec!["--received-option", "--different-received-option", "--last-received-option"]
    }
    
    #[test]
    fn test_parse_arg_doesnt_find_missing() {
        assert_eq!(parse_arg::<String>("--searched-option", &arg_set_1(), &empty()),
                   None);
    }
    
    #[test]
    fn test_parse_arg_finds_only() {
        let args = vec!["--received-option".to_string(), "value".to_string()];
        
        assert_eq!(parse_arg::<String>("--received-option", &args, &empty()),
                   Some("value".to_string()));
    }
    
    #[test]
    fn test_parse_arg_finds_first() {
        assert_eq!(parse_arg::<String>("--received-option", &arg_set_1(), &empty()),
                   Some("value1".to_string()));
    }
    
    #[test]
    fn test_parse_arg_finds_mid() {
        assert_eq!(parse_arg::<String>("--different-received-option", &arg_set_1(), &empty()),
                   Some("value2".to_string()));
    }
    
    #[test]
    fn test_parse_arg_finds_last() {
        assert_eq!(parse_arg::<String>("--last-received-option", &arg_set_1(), &empty()),
                   Some("value3".to_string()));
    }
    
    #[test]
    fn test_parse_arg_finds_tricky() {
        assert_eq!(parse_arg::<String>("--different-received-option",
                                       &arg_set_2(), &accepted_options()),
                   Some("value2".to_string()));
    }
}
