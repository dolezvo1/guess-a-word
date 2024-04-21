
use std::io::{Read, Write};
use serde::{Serialize, Deserialize};
use std::sync::mpsc::Sender;
use tokio::task::{spawn_blocking, JoinHandle};
use bincode::ErrorKind;

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
        } else if let Some(_) = options.iter().find(|e| *e == value) {
            it.nth(1);
        }
    }
    None
}

/// Client may either be Free (not in a game), or 
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ClientState {
    Free,
    Riddlemaker(String),
    Guesser,
}

/// Internal message type - type of messages flowing between threads on one machine
pub enum InternalMessage<T> {
    Network(Result<T,()>),
    Opponent(T),
    OpponentAssigned(/*tx: */Sender<InternalMessage<T>>, /*id:*/String, /*word: */String),
    WordFound,
    OpponentDisconnected,
    StdIn(String),
    Error,
}

/// Protocol of the game - type of messages flowing between machines
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum GuessProtocol {
    // "Upon connection - the server must send a message to the client - initiating the communication. Client upon receiving it - answers to the server with a password."
    ServerHello,
    ClientPassword(String),
    // This initial exchange then ends with server either disconnecting the client (wrong password) or assigning the client an ID and sending the ID back to the client.
    ConnectionEstablished(/*id: */String),
    
    // 1. Client A requests a list of possible opponents (IDs)
    ClientListOpponents,
    // 2. Server responds with a list of possible opponents (IDs)
    ServerOpponentList(/*opponents_ids:*/Vec<String>),
    // 3. Client A requests a match with opponent (ID), specifying a word to guess
    ClientSelectOpponent(/*id: */String, /*word: */String),
    // 4. Server either confirms this or rejects with an error code
    // 5. The target client - client B - is informed of the match, and can begin guesses
    OpponentConnectionEstablished(/*id: */String, /*new state: */ClientState),
    OpponentConnectionNotEstablished(/*id: */String),
    // 6. Client A is informed of the progress of Client B (attempts)
    ClientGuess(/*word: */String),
    // 7. Client A can write an arbitrary text (a hint) that is sent to and displayed by Client B
    ClientHint(/*word: */String),
    // 8. Match ends when Client B guesses the word, or gives up
    ServerWordFound,
    ServerOpponentDisconnected,
    
    UnrecognizedMessageError,
    LogicError,
}

// Traits for reading and writing messages of some protocol
//   both of these operations should be blocking
pub trait ProtocolReader<T>
    where T: for<'a> Deserialize<'a>
{
    fn read(&self) -> Result<T, Box<ErrorKind>>;
}
pub trait ProtocolWriter<T>
    where T: Serialize
{
    fn write(&self, element: &T) -> Result<(), Box<ErrorKind>>;
}

// Implement traits above for any object that is Read and/or Write
// Beware, the provided operations are blocking only as long as the underlying structs are.
// For non-blocking operations use a separate thread and a channel.
impl<T, U> ProtocolReader<T> for U
    where T: for<'a> Deserialize<'a> + std::fmt::Debug, for<'a> &'a U: Read
{
    fn read(&self) -> Result<T, Box<ErrorKind>> {
        bincode::deserialize_from(self)
    }
}
impl<T, U> ProtocolWriter<T> for U
    where T: Serialize + std::fmt::Debug, for<'a> &'a U: Write
{
    fn write(&self, element: &T) -> Result<(), Box<ErrorKind>> {
        bincode::serialize_into(self, element)
    }
}

/// Spawn a task to block on network reader
/// Sends InternalMessage::Network(Ok(a)) when message is read,
///       InternalMessage::Network(Err(())) when message deserialization fails,
///   and InternalMessage::Error on unrecoverable error (such as lost connection).
pub fn spawn_network_listener<R, T>(
    reader: Box<R>,
    transmitter: Sender<InternalMessage<T>>,
) -> JoinHandle<()>
    where R: ProtocolReader<T> + Send + ?Sized + 'static,
          T: for<'a> Deserialize<'a> + Send + 'static
{
    spawn_blocking(move || { loop {
        match reader.read() {
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
