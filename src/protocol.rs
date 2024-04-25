
use std::io::{Read, Write};
use serde::{Serialize, Deserialize};
use bincode::ErrorKind;

/// Client may either be Free (not in a game), or in one of the role states
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ClientState {
    Free,
    Riddlemaker(String),
    Guesser,
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
    fn read(&mut self) -> Result<T, Box<ErrorKind>>;
}
pub trait ProtocolWriter<T>
    where T: Serialize
{
    fn write(&mut self, element: &T) -> Result<(), Box<ErrorKind>>;
}

// Implement traits above for any object that is Read and/or Write
// Beware, the provided operations are blocking only as long as the underlying structs are.
// For non-blocking operations use a separate thread and a channel.
impl<T, U> ProtocolReader<T> for U
    where T: for<'a> Deserialize<'a> + std::fmt::Debug,
          U: Read + ?Sized
{
    fn read(&mut self) -> Result<T, Box<ErrorKind>> {
        bincode::deserialize_from(self)
    }
}
impl<T, U> ProtocolWriter<T> for U
    where T: Serialize + std::fmt::Debug,
          U: Write + ?Sized
{
    fn write(&mut self, element: &T) -> Result<(), Box<ErrorKind>> {
        bincode::serialize_into(self, element)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use crate::Ptcl;
    
    #[test]
    fn test_is_generally_shorter_than_json() {
        // Prepare
        let mut dest = VecDeque::<u8>::new();
        
        // Execute
        let _ = ProtocolWriter::write(&mut dest, &Ptcl::ClientListOpponents);
        
        // Test
        assert!(dest.len() < r#"{"type": "list_opponents"}"#.len());
        assert!(dest.len() < r#"{"t":"l"}"#.len());
        assert!(dest.len() <= r#"[14]"#.len());
    }
}