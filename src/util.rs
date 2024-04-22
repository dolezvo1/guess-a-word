
use std::io::{Read, Write};
use serde::{Serialize, Deserialize};
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
        } else if options.iter().any(|e| *e == value) {
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

/// Spawn a task to block on network reader read
/// Sends Self::Network(Ok(a)) when message is read,
///       Self::Network(Err(())) when message deserialization fails,
///   and Self::Error on unrecoverable error (such as lost connection).
macro_rules! impl_spawn_network_listener {($vis: vis) => {
$vis fn spawn_network_listener<R>(
    reader: Box<R>,
    transmitter: Sender<Self>,
) -> JoinHandle<()>
    where R: ProtocolReader<T> + Send + ?Sized + 'static,
          T: for<'a> Deserialize<'a> + Send + 'static
{
    spawn_blocking(move || { loop {
        match reader.read() {
            Ok(a) => {
                transmitter.send(Self::Network(Ok(a))).unwrap();
            },
            Err(e) => match *e {
                ErrorKind::Io(_) => {
                    transmitter.send(Self::Error).unwrap();
                    break;
                },
                _ => {
                    transmitter.send(Self::Network(Err(()))).unwrap();
                },
            }
        }
    }})
}
}}
pub(crate) use impl_spawn_network_listener;

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
